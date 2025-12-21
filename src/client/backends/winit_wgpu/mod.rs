// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;
use std::thread;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::dpi::PhysicalPosition;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::Cursor;
use winit::window::CursorIcon;
use winit::window::ResizeDirection;
use winit::window::Window;
use winit::window::WindowLevel;

use calloop::EventLoop as CalloopEventLoop;
use calloop::channel::Event as CalloopChannelEvent;
use tracing::{debug, info, warn};

use crate::client::config::KeyboardMode;
use crate::client::coords;
use crate::client::coords::ServerBufferScale;
use crate::client::coords::UiScaleFactor;
use crate::filtering;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::ClientId;
use crate::protocols::wprs::DisplayConfig;
use crate::protocols::wprs::RecvType;
use crate::protocols::wprs::Request;
use crate::protocols::wprs::SendType;
use crate::protocols::wprs::Serializer;
use crate::protocols::wprs::geometry::{Point, Size};
use crate::protocols::wprs::wayland::ClientSurface;
use crate::protocols::wprs::wayland::PointerGestureEvent;
use crate::protocols::wprs::wayland::{
    AxisScroll, AxisSource, KeyInner, KeyState, KeyboardEvent, ModifierState, PointerEvent,
    PointerEventKind,
};
use crate::protocols::wprs::wayland::{
    BufferAssignment, BufferData, Mode, OutputEvent, OutputInfo, Subpixel, SurfaceRequest,
    SurfaceRequestPayload, Transform, UncompressedBufferData, WlSurfaceId,
};
use crate::protocols::wprs::xdg_shell::XdgPopupState;
use crate::protocols::wprs::xdg_shell::{
    DecorationMode, ToplevelClose, ToplevelConfigure, ToplevelEvent, WindowState,
};

#[derive(Debug, Clone, Copy, Default)]
struct PinchGestureState {
    active_pinch: bool,
    active_rotation: bool,
    scale: f64,
}

#[derive(Clone, Debug)]
pub struct WinitWgpuOptions {
    pub keyboard_mode: KeyboardMode,
    pub xkb_keymap_file: Option<std::path::PathBuf>,
    pub ui_scale_factor: f64,
    pub min_output_scale_factor: i32,
}

#[derive(Debug)]
pub enum UserEvent {
    ServerMessage(RecvType<Request>),
    DecodedFrame(DecodedFrame),
}

#[derive(Debug)]
pub struct DecodedFrame {
    pub surface_id: WlSurfaceId,
    pub metadata: crate::protocols::wprs::wayland::BufferMetadata,
    pub padded_row_bytes: u32,
    pub data: Vec<u8>,
}

#[derive(Debug)]
struct DecodeJob {
    surface_id: WlSurfaceId,
    metadata: crate::protocols::wprs::wayland::BufferMetadata,
    filtered: crate::vec4u8::Vec4u8s,
}

#[derive(Clone)]
struct WgpuShared {
    instance: Arc<wgpu::Instance>,
    adapter: Arc<wgpu::Adapter>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

struct WindowRenderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,

    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bind_group: Option<wgpu::BindGroup>,
    texture: Option<wgpu::Texture>,
    texture_size: Option<(u32, u32)>,
}

// See ../winit/winit/examples/custom_decorations.rs for the baseline approach.
//
// In wprs, remote surfaces may draw their own UI in the top region of the window, so we avoid
// taking over a fixed “titlebar area” for click-to-drag. Instead:
// - Edge/corner resize is enabled in decorationless mode.
// - Window move is enabled via Alt/Option + left-drag.
const DECORATIONLESS_RESIZE_BORDER_LOGICAL: f64 = 8.0;

impl WindowRenderer {

    fn new(shared: &WgpuShared, window: Arc<Window>) -> Result<Self> {
        let surface = shared
            .instance
            .create_surface(window.clone())
            .location(loc!())?;

        let caps = surface.get_capabilities(&shared.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: caps.present_modes[0],
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&shared.device, &config);

        let shader = shared
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("wprs_winit_wgpu_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
            });

        let bind_group_layout =
            shared
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("wprs_winit_wgpu_bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let sampler = shared.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wprs_winit_wgpu_sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout =
            shared
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("wprs_winit_wgpu_pipeline_layout"),
                    bind_group_layouts: &[&bind_group_layout],
                    push_constant_ranges: &[],
                });

        let pipeline = shared
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("wprs_winit_wgpu_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: 16,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 8,
                                shader_location: 1,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        // Render the remote surface across the full window. On macOS we use full-size-content-view
        // so the content can appear behind the (transparent) titlebar.
        let vertex_data = Self::vertex_data_for_top_inset(config.height, 0);
        let vertex_buffer = shared.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wprs_winit_wgpu_vertex_buffer"),
            size: (vertex_data.len() * std::mem::size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        shared
            .queue
            .write_buffer(&vertex_buffer, 0, bytemuck::cast_slice(&vertex_data));

        Ok(Self {
            window,
            surface,
            config,
            pipeline,
            vertex_buffer,
            bind_group_layout,
            sampler,
            bind_group: None,
            texture: None,
            texture_size: None,
        })
    }

    fn vertex_data_for_top_inset(height_px: u32, top_inset_px: u32) -> [f32; 24] {
        let height = height_px.max(1) as f32;
        let inset = top_inset_px.min(height_px.saturating_sub(1)) as f32;
        let content_top = 1.0 - 2.0 * (inset / height);

        [
            // pos(x,y) uv(u,v)
            -1.0,
            -1.0,
            0.0,
            1.0, //
            1.0,
            -1.0,
            1.0,
            1.0, //
            1.0,
            content_top,
            1.0,
            0.0, //
            -1.0,
            -1.0,
            0.0,
            1.0, //
            1.0,
            content_top,
            1.0,
            0.0, //
            -1.0,
            content_top,
            0.0,
            0.0, //
        ]
    }

    fn update_vertices(&mut self, shared: &WgpuShared) {
        let vertex_data = Self::vertex_data_for_top_inset(self.config.height, 0);
        shared
            .queue
            .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertex_data));
    }

    fn resize(&mut self, shared: &WgpuShared, size: PhysicalSize<u32>) {
        self.config.width = size.width.max(1);
        self.config.height = size.height.max(1);
        self.surface.configure(&shared.device, &self.config);
        self.update_vertices(shared);
    }

    fn update_texture_from_padded_bgra(
        &mut self,
        shared: &WgpuShared,
        metadata: &crate::protocols::wprs::wayland::BufferMetadata,
        padded_row_bytes: u32,
        padded_data: &[u8],
    ) {
        let width = metadata.width as u32;
        let height = metadata.height as u32;

        if self.texture_size != Some((width, height)) {
            let texture = shared.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wprs_remote_texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = shared.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wprs_remote_texture_bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.texture = Some(texture);
            self.bind_group = Some(bind_group);
            self.texture_size = Some((width, height));
        }

        let texture = self.texture.as_ref().unwrap();
        shared.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            padded_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row_bytes),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn render(&mut self, shared: &WgpuShared) -> Result<()> {
        debug_assert_eq!(self.bind_group.is_some(), self.texture.is_some());
        debug_assert_eq!(self.bind_group.is_some(), self.texture_size.is_some());

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Outdated) | Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&shared.device, &self.config);
                return Ok(());
            },
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(err) => return Err(anyhow!("surface acquire failed: {err:?}")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = shared
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wprs_winit_wgpu_encoder"),
            });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wprs_winit_wgpu_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // The pipeline expects a texture/sampler bind group at index 0. Avoid issuing draw
            // calls before we've received the first remote frame (or if the texture was reset).
            if let Some(bind_group) = &self.bind_group {
                rp.set_pipeline(&self.pipeline);
                rp.set_bind_group(0, bind_group, &[]);
                rp.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                rp.draw(0..6, 0..1);
            }
        }
        shared.queue.submit([encoder.finish()]);
        frame.present();
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
struct ClientSurfaceKey {
    client: ClientId,
    surface: WlSurfaceId,
}

impl ClientSurfaceKey {
    fn new(client_surface: &ClientSurface) -> Self {
        Self {
            client: client_surface.client,
            surface: client_surface.surface,
        }
    }
}

#[derive(Debug, Clone)]
struct CursorFrame {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
}

fn align_up(value: usize, alignment: usize) -> usize {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

fn decode_filtered_to_padded_bgra(
    metadata: &crate::protocols::wprs::wayland::BufferMetadata,
    filtered: &crate::vec4u8::Vec4u8s,
) -> (u32, Vec<u8>) {
    let width = metadata.width as usize;
    let height = metadata.height as usize;
    let src_stride = metadata.stride as usize;
    let row_bytes = width * 4;

    let mut unfiltered = vec![0u8; metadata.len()];
    filtering::unfilter(filtered, &mut unfiltered);

    let padded_row_bytes = align_up(row_bytes, 256);
    let mut padded = vec![0u8; padded_row_bytes * height];
    for y in 0..height {
        let src = &unfiltered[y * src_stride..y * src_stride + row_bytes];
        let dst = &mut padded[y * padded_row_bytes..y * padded_row_bytes + row_bytes];
        dst.copy_from_slice(src);
    }

    (padded_row_bytes as u32, padded)
}

fn output_info_from_monitor(
    id: u32,
    monitor: &winit::monitor::MonitorHandle,
    min_output_scale_factor: i32,
) -> OutputInfo {
    let name = monitor.name();
    let mut scale_factor = monitor.scale_factor().round() as i32;
    scale_factor = scale_factor.max(1);

    let position = monitor.position();
    let size = monitor.size();

    // Force a HiDPI scale on macOS to avoid blurry rendering when the server uses a low scale.
    // We keep the logical size stable by scaling both the mode dimensions and the scale factor.
    #[cfg(target_os = "macos")]
    let (position, size, scale_factor) = {
        let desired_scale = scale_factor.max(min_output_scale_factor);
        let multiplier = (desired_scale / scale_factor).max(1);
        (
            winit::dpi::PhysicalPosition::new(position.x * multiplier, position.y * multiplier),
            winit::dpi::PhysicalSize::new(
                size.width * multiplier as u32,
                size.height * multiplier as u32,
            ),
            desired_scale,
        )
    };
    OutputInfo {
        id,
        model: name.clone().unwrap_or_default(),
        make: String::new(),
        location: Point {
            x: position.x,
            y: position.y,
        },
        physical_size: Size { w: 0, h: 0 },
        subpixel: Subpixel::Unknown,
        transform: Transform::Normal,
        scale_factor,
        mode: Mode {
            dimensions: Size {
                w: size.width as i32,
                h: size.height as i32,
            },
            refresh_rate: 60_000,
            current: true,
            preferred: true,
        },
        name,
        description: None,
    }
}

struct App {
    shared: WgpuShared,
    serializer: Serializer<proto::Event, Request>,
    decode_tx: std::sync::mpsc::Sender<DecodeJob>,
    buffer_cache: Option<UncompressedBufferData>,
    windows: HashMap<WlSurfaceId, WindowRenderer>,
    surface_by_window: HashMap<winit::window::WindowId, WlSurfaceId>,
    outputs_sent: bool,

    last_outputs_refresh: Instant,
    last_outputs: Vec<OutputInfo>,
    min_output_scale_factor: i32,

    server_display_config: Option<DisplayConfig>,
    surface_scale_factor: HashMap<WlSurfaceId, i32>,

    keyboard_mode: KeyboardMode,
    xkb_keymap_sent: bool,
    xkb_keymap_file: Option<std::path::PathBuf>,
    ui_scale_factor: f64,

    serial_counter: u32,
    focused_window: Option<winit::window::WindowId>,
    focused_surface: Option<WlSurfaceId>,
    surfaces_with_frame: HashSet<WlSurfaceId>,
    pressed_keycodes: HashSet<u32>,
    last_window_cursor_pos:
        HashMap<winit::window::WindowId, crate::protocols::wprs::geometry::Point<f64>>,
    last_cursor_pos: HashMap<winit::window::WindowId, crate::protocols::wprs::geometry::Point<f64>>,
    last_window_cursor_pos_physical: HashMap<winit::window::WindowId, PhysicalPosition<f64>>,
    local_modifiers: winit::keyboard::ModifiersState,
    window_has_decorations: HashMap<winit::window::WindowId, bool>,
    window_cursor_overridden: HashSet<winit::window::WindowId>,
    pointer_inside: HashSet<winit::window::WindowId>,
    pointer_surface: Option<WlSurfaceId>,

    last_window_inner_pos: HashMap<winit::window::WindowId, PhysicalPosition<i32>>,

    pinch_state: HashMap<WlSurfaceId, PinchGestureState>,

    popup_state_by_surface: HashMap<WlSurfaceId, XdgPopupState>,

    cursor_frames: HashMap<ClientSurfaceKey, CursorFrame>,
    cursor_surface_clients: HashMap<WlSurfaceId, ClientId>,

    current_cursor: Option<Cursor>,
    warned_cursor_names: HashSet<String>,
    cursor_dirty: bool,

    warned_low_buffer_scale_on_hidpi: bool,
}

impl App {
    fn refresh_outputs(&mut self, event_loop: &ActiveEventLoop, force: bool) {
        let current: Vec<OutputInfo> = event_loop
            .available_monitors()
            .enumerate()
            .map(|(idx, monitor)| {
                output_info_from_monitor(idx as u32, &monitor, self.min_output_scale_factor)
            })
            .collect();

        if !force && current == self.last_outputs {
            return;
        }

        let shared_len = current.len().min(self.last_outputs.len());
        for i in 0..shared_len {
            if current[i] != self.last_outputs[i] {
                self.serializer
                    .writer()
                    .send(SendType::Object(proto::Event::Output(OutputEvent::Update(
                        current[i].clone(),
                    ))));
            }
        }
        if current.len() > self.last_outputs.len() {
            for output in &current[self.last_outputs.len()..] {
                self.serializer
                    .writer()
                    .send(SendType::Object(proto::Event::Output(OutputEvent::New(
                        output.clone(),
                    ))));
            }
        } else if self.last_outputs.len() > current.len() {
            for output in &self.last_outputs[current.len()..] {
                self.serializer
                    .writer()
                    .send(SendType::Object(proto::Event::Output(OutputEvent::Destroy(
                        output.clone(),
                    ))));
            }
        }

        self.last_outputs = current;
    }
}

impl App {
    fn window_has_decorations(&self, window_id: winit::window::WindowId) -> bool {
        self.window_has_decorations
            .get(&window_id)
            .copied()
            .unwrap_or(true)
    }

    fn decorationless_resize_border_px(&self, window: &Window) -> f64 {
        (DECORATIONLESS_RESIZE_BORDER_LOGICAL * window.scale_factor()).max(1.0)
    }

    fn hit_test_decorationless_resize(
        &self,
        window: &Window,
        position: PhysicalPosition<f64>,
    ) -> Option<ResizeDirection> {
        let size = window.inner_size();
        let width = size.width as f64;
        let height = size.height as f64;
        if width <= 0.0 || height <= 0.0 {
            return None;
        }

        let border = self.decorationless_resize_border_px(window);
        let x = position.x;
        let y = position.y;

        let left = x >= 0.0 && x < border;
        let right = x <= width && x > width - border;
        let top = y >= 0.0 && y < border;
        let bottom = y <= height && y > height - border;

        match (left, right, top, bottom) {
            (true, _, true, _) => Some(ResizeDirection::NorthWest),
            (_, true, true, _) => Some(ResizeDirection::NorthEast),
            (true, _, _, true) => Some(ResizeDirection::SouthWest),
            (_, true, _, true) => Some(ResizeDirection::SouthEast),
            (true, _, _, _) => Some(ResizeDirection::West),
            (_, true, _, _) => Some(ResizeDirection::East),
            (_, _, true, _) => Some(ResizeDirection::North),
            (_, _, _, true) => Some(ResizeDirection::South),
            _ => None,
        }
    }

    fn cursor_icon_for_resize(dir: ResizeDirection) -> CursorIcon {
        match dir {
            ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
            ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
            ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
            ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
        }
    }

    fn update_decorationless_cursor(
        &mut self,
        surface_id: WlSurfaceId,
        window_id: winit::window::WindowId,
        window: &Window,
        position_physical: PhysicalPosition<f64>,
    ) {
        if self.window_has_decorations(window_id) {
            return;
        }

        let resize_dir = self.hit_test_decorationless_resize(window, position_physical);
        let want_move_cursor = self.local_modifiers.alt_key() && resize_dir.is_none();

        if let Some(dir) = resize_dir {
            window.set_cursor(Cursor::from(Self::cursor_icon_for_resize(dir)));
            self.window_cursor_overridden.insert(window_id);
            return;
        }

        if want_move_cursor {
            window.set_cursor(Cursor::from(CursorIcon::Move));
            self.window_cursor_overridden.insert(window_id);
            return;
        }

        if self.window_cursor_overridden.remove(&window_id) {
            self.apply_cursor_for_surface(surface_id);
        }
    }

    fn ui_scale(&self) -> f64 {
        self.ui_scale_factor.max(0.1)
    }

    fn schedule_decode(
        &self,
        surface_id: WlSurfaceId,
        metadata: crate::protocols::wprs::wayland::BufferMetadata,
        filtered: crate::vec4u8::Vec4u8s,
    ) {
        // If the receiver is gone, we are shutting down.
        let _ = self.decode_tx.send(DecodeJob {
            surface_id,
            metadata,
            filtered,
        });
    }

    fn bgra_padded_to_rgba(
        metadata: &crate::protocols::wprs::wayland::BufferMetadata,
        padded_row_bytes: u32,
        padded: &[u8],
    ) -> Option<CursorFrame> {
        let width: usize = metadata.width.try_into().ok()?;
        let height: usize = metadata.height.try_into().ok()?;
        let width_u16: u16 = metadata.width.try_into().ok()?;
        let height_u16: u16 = metadata.height.try_into().ok()?;
        if width == 0 || height == 0 {
            return None;
        }

        let padded_row_bytes = padded_row_bytes as usize;
        let row_bytes = width.checked_mul(4)?;
        if padded_row_bytes < row_bytes {
            return None;
        }
        let total = padded_row_bytes.checked_mul(height)?;
        if padded.len() < total {
            return None;
        }

        let mut rgba = vec![0u8; row_bytes * height];
        for y in 0..height {
            let src = &padded[y * padded_row_bytes..y * padded_row_bytes + row_bytes];
            let dst = &mut rgba[y * row_bytes..y * row_bytes + row_bytes];
            for x in 0..width {
                let s = x * 4;
                // BGRA -> RGBA
                dst[s] = src[s + 2];
                dst[s + 1] = src[s + 1];
                dst[s + 2] = src[s];
                dst[s + 3] = src[s + 3];
            }
        }

        Some(CursorFrame {
            width: width_u16,
            height: height_u16,
            rgba,
        })
    }

    fn compute_popup_position(&self, popup: &XdgPopupState) -> Option<PhysicalPosition<i32>> {
        let parent_renderer = self.windows.get(&popup.parent_surface_id)?;
        let parent_window_id = parent_renderer.window.id();
        let parent_pos = self
            .last_window_inner_pos
            .get(&parent_window_id)
            .copied()
            .or_else(|| parent_renderer.window.inner_position().ok())
            .or_else(|| parent_renderer.window.outer_position().ok())?;

        // The positioner is expressed in the parent's surface coordinate space.
        // Map it into a global coordinate space using the parent's outer position.
        let anchor = popup.positioner.anchor_rect;
        let offset = popup.positioner.offset;

        let server_scale = self
            .surface_scale_factor
            .get(&popup.parent_surface_id)
            .copied()
            .unwrap_or(1);
        let dx_server = anchor.loc.x + offset.x;
        let dy_server = anchor.loc.y + offset.y;

        let (dx, dy) = coords::winit::popup_offset_to_host_px(
            parent_renderer.window.as_ref(),
            UiScaleFactor(self.ui_scale_factor),
            ServerBufferScale(server_scale),
            dx_server,
            dy_server,
        );

        Some(PhysicalPosition::new(
            parent_pos.x.saturating_add(dx),
            parent_pos.y.saturating_add(dy),
        ))
    }

    fn update_popup_position(&self, popup_surface_id: WlSurfaceId, popup: &XdgPopupState) {
        let Some(renderer) = self.windows.get(&popup_surface_id) else {
            return;
        };
        let Some(pos) = self.compute_popup_position(popup) else {
            return;
        };
        renderer.window.set_outer_position(pos);
    }

    fn update_popups_for_parent(&self, parent_surface_id: WlSurfaceId) {
        for (popup_surface_id, popup) in &self.popup_state_by_surface {
            if popup.parent_surface_id == parent_surface_id {
                self.update_popup_position(*popup_surface_id, popup);
            }
        }
    }

    fn keyboard_focus_target_for(&self, surface_id: WlSurfaceId) -> WlSurfaceId {
        // In XDG shell, keyboard focus remains with the toplevel. Popups are never keyboard focus
        // targets.
        let mut target = surface_id;
        while let Some(popup) = self.popup_state_by_surface.get(&target) {
            target = popup.parent_surface_id;
        }
        target
    }

    fn cursor_icon_from_wayland_name(name: &str) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;

        // Prefer parsing the standard cursor-icon names (lower kebab case).
        let lowered = name.to_ascii_lowercase();
        if let Ok(icon) = lowered.parse::<CursorIcon>() {
            return Some(icon);
        }
        let normalized = lowered.replace('_', "-");
        if let Ok(icon) = normalized.parse::<CursorIcon>() {
            return Some(icon);
        }

        // Fall back to common Xcursor theme aliases.
        Some(match lowered.as_str() {
            "left_ptr" | "arrow" => CursorIcon::Default,
            "hand" | "hand1" | "hand2" => CursorIcon::Pointer,
            "xterm" | "ibeam" => CursorIcon::Text,
            "cross" => CursorIcon::Crosshair,
            "fleur" => CursorIcon::Move,
            "left_ptr_watch" => CursorIcon::Progress,
            "watch" => CursorIcon::Wait,
            "question_arrow" => CursorIcon::Help,
            "forbidden" => CursorIcon::NotAllowed,

            "sb_h_double_arrow" => CursorIcon::ColResize,
            "sb_v_double_arrow" => CursorIcon::RowResize,

            _ => return None,
        })
    }

    fn apply_cursor_for_surface(&self, surface_id: WlSurfaceId) {
        let Some(renderer) = self.windows.get(&surface_id) else {
            return;
        };

        match &self.current_cursor {
            None => renderer.window.set_cursor_visible(false),
            Some(cursor) => {
                renderer.window.set_cursor_visible(true);
                renderer.window.set_cursor(cursor.clone());
            },
        }
    }

    fn handle_cursor_image(
        &mut self,
        event_loop: &ActiveEventLoop,
        cursor: crate::protocols::wprs::wayland::CursorImage,
    ) {
        let serial = cursor.serial;
        let status = cursor.status;

        // Cursor is a seat-global concept; we apply it to whichever surface is currently active.
        // If we don't have an active surface yet, mark it dirty and apply on next pointer enter.
        let target_surface = self.pointer_surface.or(self.focused_surface);
        match status {
            crate::protocols::wprs::wayland::CursorImageStatus::Hidden => {
                debug!("cursor hidden: serial={serial}");
                self.current_cursor = None;
                self.cursor_dirty = true;
            },
            crate::protocols::wprs::wayland::CursorImageStatus::Named(name) => {
                let icon = Self::cursor_icon_from_wayland_name(&name);
                if icon.is_none() && self.warned_cursor_names.insert(name.clone()) {
                    warn!("unhandled cursor icon name {name:?}; falling back to default");
                }
                let icon = icon.unwrap_or(winit::window::CursorIcon::Default);
                debug!(
                    "cursor named: serial={} name={name:?} icon={icon:?}",
                    serial
                );
                self.current_cursor = Some(Cursor::from(icon));
                self.cursor_dirty = true;
            },
            crate::protocols::wprs::wayland::CursorImageStatus::Surface {
                client_surface,
                hotspot,
            } => {
                let key = ClientSurfaceKey::new(&client_surface);
                let Some(frame) = self.cursor_frames.get(&key) else {
                    debug!("cursor surface: serial={serial} cursor_surface={key:?} (no frame yet)");
                    return;
                };

                let hotspot_x = hotspot.x.clamp(0, frame.width as i32) as u16;
                let hotspot_y = hotspot.y.clamp(0, frame.height as i32) as u16;

                // Smithay reports hotspot in surface coordinates; the cursor surface may have a
                // non-1 buffer scale (HiDPI). winit expects hotspot in cursor image pixels.
                let buffer_scale = self
                    .surface_scale_factor
                    .get(&key.surface)
                    .copied()
                    .unwrap_or(1)
                    .max(1);
                let hotspot_x =
                    (i32::from(hotspot_x) * buffer_scale).clamp(0, frame.width as i32) as u16;
                let hotspot_y =
                    (i32::from(hotspot_y) * buffer_scale).clamp(0, frame.height as i32) as u16;

                let source = match winit::window::CustomCursor::from_rgba(
                    frame.rgba.clone(),
                    frame.width,
                    frame.height,
                    hotspot_x,
                    hotspot_y,
                ) {
                    Ok(source) => source,
                    Err(err) => {
                        debug!(
                            "cursor surface: failed to create custom cursor: serial={serial} err={err:?}"
                        );
                        return;
                    },
                };
                let custom = event_loop.create_custom_cursor(source);
                debug!(
                    "cursor surface: serial={serial} cursor_surface={key:?} size=({}x{}) hotspot=({hotspot_x},{hotspot_y})",
                    frame.width, frame.height
                );
                self.current_cursor = Some(Cursor::from(custom));
                self.cursor_dirty = true;
            },
        }

        if let Some(surface_id) = target_surface {
            self.apply_cursor_for_surface(surface_id);
            self.cursor_dirty = false;
        }
    }

    fn generate_keymap_from_tools() -> Result<String> {
        // Preferred path: ask X11 for the active keymap (works under Xwayland/X11).
        // `setxkbmap -print` emits an XKB config, `xkbcomp -xkb - -` compiles it to a keymap.
        let setxkbmap_output = std::process::Command::new("setxkbmap")
            .args(["-print"])
            .output()
            .location(loc!())?;
        if !setxkbmap_output.status.success() {
            bail!("setxkbmap -print failed: {:?}", setxkbmap_output.status);
        }

        let mut xkbcomp = std::process::Command::new("xkbcomp")
            .args(["-xkb", "-", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .location(loc!())?;

        {
            let stdin = xkbcomp.stdin.as_mut().location(loc!())?;
            use std::io::Write as _;
            stdin.write_all(&setxkbmap_output.stdout).location(loc!())?;
        }

        let output = xkbcomp.wait_with_output().location(loc!())?;
        if !output.status.success() {
            bail!(
                "xkbcomp -xkb - - failed: {:?}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8(output.stdout).location(loc!())?)
    }

    fn maybe_send_keymap(&mut self) {
        if self.xkb_keymap_sent {
            return;
        }
        self.xkb_keymap_sent = true;

        if self.keyboard_mode != KeyboardMode::Keymap {
            return;
        }

        let keymap = if let Some(path) = self.xkb_keymap_file.as_ref() {
            match std::fs::read_to_string(path).with_context(loc!(), || {
                format!("failed to read xkb keymap file {path:?}")
            }) {
                Ok(keymap) => keymap,
                Err(err) => {
                    warn!("{err:?}; continuing with evdev mapping");
                    return;
                },
            }
        } else {
            match Self::generate_keymap_from_tools() {
                Ok(keymap) => keymap,
                Err(err) => {
                    warn!(
                        "failed to generate xkb keymap via tools: {err:?}; continuing with evdev mapping"
                    );
                    return;
                },
            }
        };

        self.serializer
            .writer()
            .send(SendType::Object(proto::Event::KeyboardEvent(
                KeyboardEvent::Keymap(keymap),
            )));
    }
    fn next_serial(&mut self) -> u32 {
        self.serial_counter = self.serial_counter.wrapping_add(1);
        if self.serial_counter == 0 {
            self.serial_counter = 1;
        }
        self.serial_counter
    }

    fn cursor_pos_for(
        &self,
        window_id: winit::window::WindowId,
    ) -> crate::protocols::wprs::geometry::Point<f64> {
        self.last_cursor_pos
            .get(&window_id)
            .copied()
            .unwrap_or(crate::protocols::wprs::geometry::Point { x: 0.0, y: 0.0 })
    }

    fn send_pointer_event(
        &mut self,
        surface_id: WlSurfaceId,
        position: crate::protocols::wprs::geometry::Point<f64>,
        kind: PointerEventKind,
    ) {
        self.serializer
            .writer()
            .send(SendType::Object(proto::Event::PointerFrame(vec![
                PointerEvent {
                    surface_id,
                    position,
                    kind,
                },
            ])));
    }

    fn set_keyboard_focus(&mut self, surface_id: Option<WlSurfaceId>) {
        self.maybe_send_keymap();
        if self.focused_surface == surface_id {
            return;
        }
        if self.focused_surface.is_some() {
            let serial = self.next_serial();
            self.serializer
                .writer()
                .send(SendType::Object(proto::Event::KeyboardEvent(
                    KeyboardEvent::Leave { serial },
                )));
        }

        self.focused_surface = surface_id;

        if let Some(surface_id) = surface_id {
            let mut keycodes: Vec<u32> = self.pressed_keycodes.iter().copied().collect();
            keycodes.sort_unstable();
            let serial = self.next_serial();
            self.serializer
                .writer()
                .send(SendType::Object(proto::Event::KeyboardEvent(
                    KeyboardEvent::Enter {
                        serial,
                        surface_id,
                        keycodes,
                        keysyms: Vec::new(),
                    },
                )));
        }
    }

    fn send_modifiers(&mut self, modifiers: winit::keyboard::ModifiersState) {
        self.serializer
            .writer()
            .send(SendType::Object(proto::Event::KeyboardEvent(
                KeyboardEvent::Modifiers {
                    modifier_state: ModifierState {
                        ctrl: modifiers.control_key(),
                        alt: modifiers.alt_key(),
                        shift: modifiers.shift_key(),
                        // winit doesn't expose lock states in ModifiersState.
                        caps_lock: false,
                        logo: modifiers.super_key(),
                        num_lock: false,
                    },
                    layout_index: 0,
                },
            )));
    }

    fn send_key(&mut self, keycode: u32, state: KeyState) {
        // Keyboard events are applied to the currently-focused surface on the server side.
        if self.focused_surface.is_none() {
            return;
        }
        let serial = self.next_serial();
        self.serializer
            .writer()
            .send(SendType::Object(proto::Event::KeyboardEvent(
                KeyboardEvent::Key(KeyInner {
                    serial,
                    raw_code: keycode,
                    state,
                }),
            )));
    }

    fn linux_button_from_winit(button: MouseButton) -> Option<u32> {
        // linux/input-event-codes.h
        match button {
            MouseButton::Left => Some(272),
            MouseButton::Right => Some(273),
            MouseButton::Middle => Some(274),
            MouseButton::Back => Some(275),
            MouseButton::Forward => Some(276),
            MouseButton::Other(_) => None,
        }
    }

    fn linux_keycode_from_winit(code: winit::keyboard::KeyCode) -> Option<u32> {
        use winit::keyboard::KeyCode;

        // linux/input-event-codes.h keycodes (evdev)
        Some(match code {
            KeyCode::Escape => 1,
            KeyCode::Digit1 => 2,
            KeyCode::Digit2 => 3,
            KeyCode::Digit3 => 4,
            KeyCode::Digit4 => 5,
            KeyCode::Digit5 => 6,
            KeyCode::Digit6 => 7,
            KeyCode::Digit7 => 8,
            KeyCode::Digit8 => 9,
            KeyCode::Digit9 => 10,
            KeyCode::Digit0 => 11,
            KeyCode::Minus => 12,
            KeyCode::Equal => 13,
            KeyCode::Backspace => 14,
            KeyCode::Tab => 15,
            KeyCode::KeyQ => 16,
            KeyCode::KeyW => 17,
            KeyCode::KeyE => 18,
            KeyCode::KeyR => 19,
            KeyCode::KeyT => 20,
            KeyCode::KeyY => 21,
            KeyCode::KeyU => 22,
            KeyCode::KeyI => 23,
            KeyCode::KeyO => 24,
            KeyCode::KeyP => 25,
            KeyCode::BracketLeft => 26,
            KeyCode::BracketRight => 27,
            KeyCode::Enter => 28,
            KeyCode::ControlLeft => 29,
            KeyCode::KeyA => 30,
            KeyCode::KeyS => 31,
            KeyCode::KeyD => 32,
            KeyCode::KeyF => 33,
            KeyCode::KeyG => 34,
            KeyCode::KeyH => 35,
            KeyCode::KeyJ => 36,
            KeyCode::KeyK => 37,
            KeyCode::KeyL => 38,
            KeyCode::Semicolon => 39,
            KeyCode::Quote => 40,
            KeyCode::Backquote => 41,
            KeyCode::ShiftLeft => 42,
            KeyCode::Backslash => 43,
            KeyCode::KeyZ => 44,
            KeyCode::KeyX => 45,
            KeyCode::KeyC => 46,
            KeyCode::KeyV => 47,
            KeyCode::KeyB => 48,
            KeyCode::KeyN => 49,
            KeyCode::KeyM => 50,
            KeyCode::Comma => 51,
            KeyCode::Period => 52,
            KeyCode::Slash => 53,
            KeyCode::ShiftRight => 54,
            KeyCode::NumpadMultiply => 55,
            KeyCode::AltLeft => 56,
            KeyCode::Space => 57,
            KeyCode::CapsLock => 58,
            KeyCode::F1 => 59,
            KeyCode::F2 => 60,
            KeyCode::F3 => 61,
            KeyCode::F4 => 62,
            KeyCode::F5 => 63,
            KeyCode::F6 => 64,
            KeyCode::F7 => 65,
            KeyCode::F8 => 66,
            KeyCode::F9 => 67,
            KeyCode::F10 => 68,
            KeyCode::NumLock => 69,
            KeyCode::ScrollLock => 70,
            KeyCode::Numpad7 => 71,
            KeyCode::Numpad8 => 72,
            KeyCode::Numpad9 => 73,
            KeyCode::NumpadSubtract => 74,
            KeyCode::Numpad4 => 75,
            KeyCode::Numpad5 => 76,
            KeyCode::Numpad6 => 77,
            KeyCode::NumpadAdd => 78,
            KeyCode::Numpad1 => 79,
            KeyCode::Numpad2 => 80,
            KeyCode::Numpad3 => 81,
            KeyCode::Numpad0 => 82,
            KeyCode::NumpadDecimal => 83,
            KeyCode::F11 => 87,
            KeyCode::F12 => 88,
            KeyCode::NumpadEnter => 96,
            KeyCode::ControlRight => 97,
            KeyCode::NumpadDivide => 98,
            KeyCode::AltRight => 100,
            KeyCode::Home => 102,
            KeyCode::ArrowUp => 103,
            KeyCode::PageUp => 104,
            KeyCode::ArrowLeft => 105,
            KeyCode::ArrowRight => 106,
            KeyCode::End => 107,
            KeyCode::ArrowDown => 108,
            KeyCode::PageDown => 109,
            KeyCode::Insert => 110,
            KeyCode::Delete => 111,
            KeyCode::SuperLeft => 125,
            KeyCode::SuperRight => 126,
            _ => return None,
        })
    }

    fn handle_server_message(
        &mut self,
        event_loop: &ActiveEventLoop,
        msg: RecvType<Request>,
    ) -> Result<()> {
        match msg {
            RecvType::RawBuffer(buf) => {
                self.buffer_cache = Some(UncompressedBufferData(buf.into()));
                Ok(())
            },
            RecvType::Object(Request::Surface(surface)) => self.handle_surface(event_loop, surface),
            RecvType::Object(Request::DisplayConfig(cfg)) => {
                if self.server_display_config.is_none() {
                    info!(
                        "server display config: scale_factor={} dpi={:?}",
                        cfg.scale_factor, cfg.dpi
                    );
                }
                self.server_display_config = Some(cfg);
                Ok(())
            },
            RecvType::Object(Request::CursorImage(cursor)) => {
                self.handle_cursor_image(event_loop, cursor);
                Ok(())
            },
            // Not yet handled in this backend.
            _ => Ok(()),
        }
    }

    fn send_configure_for_surface(&mut self, surface_id: WlSurfaceId) {
        let Some(renderer) = self.windows.get(&surface_id) else {
            return;
        };
        let size = renderer.window.inner_size();
        let logical: winit::dpi::LogicalSize<f64> = size.to_logical(renderer.window.scale_factor());
        // Window sizes are in local logical points, but the server expects surface logical points.
        // When `ui_scale_factor` is set, we scale the view (local window size) without resizing the
        // remote surface, so we need to map back into the server's coordinate space.
        let server_logical_w = (logical.width / self.ui_scale()).round().max(1.0) as u32;
        let server_logical_h = (logical.height / self.ui_scale()).round().max(1.0) as u32;
        let configure = ToplevelConfigure {
            surface_id,
            // Configure sizes are in logical points.
            new_size: Size {
                w: NonZeroU32::new(server_logical_w),
                h: NonZeroU32::new(server_logical_h),
            },
            suggested_bounds: None,
            decoration_mode: DecorationMode::Server,
            state: WindowState::from_bits(0),
        };
        self.serializer
            .writer()
            .send(SendType::Object(proto::Event::Toplevel(
                ToplevelEvent::Configure(configure),
            )));
    }

    fn handle_surface(
        &mut self,
        event_loop: &ActiveEventLoop,
        surface: SurfaceRequest,
    ) -> Result<()> {
        let surface_id = surface.surface;
        match surface.payload {
            SurfaceRequestPayload::Destroyed => {
                if let Some(renderer) = self.windows.remove(&surface_id) {
                    self.surface_by_window.remove(&renderer.window.id());
                }
                self.popup_state_by_surface.remove(&surface_id);
                if let Some(client) = self.cursor_surface_clients.remove(&surface_id) {
                    self.cursor_frames.remove(&ClientSurfaceKey {
                        client,
                        surface: surface_id,
                    });
                }
                return Ok(());
            },
            SurfaceRequestPayload::Commit(mut state) => {
                self.surface_scale_factor
                    .insert(surface_id, state.buffer_scale.max(1));

                let Some(role) = &state.role else {
                    return Ok(());
                };
                let toplevel = role.as_xdg_toplevel();
                let popup = role.as_xdg_popup();
                let cursor = role.as_cursor();

                if cursor.is_some() {
                    self.cursor_surface_clients.insert(surface_id, state.client);
                }

                let is_presented = toplevel.is_some() || popup.is_some();
                if !is_presented && cursor.is_none() {
                    return Ok(());
                }

                if let Some(popup) = popup {
                    self.popup_state_by_surface
                        .insert(surface_id, popup.clone());
                } else {
                    self.popup_state_by_surface.remove(&surface_id);
                }

                // Ensure we have a window for this surface if it is presented.
                if is_presented && !self.windows.contains_key(&surface_id) {
                    let mut attrs = if let Some(_toplevel) = toplevel {
                        // Remote apps (e.g. KDE/Qt) may render their own client-side titlebars.
                        // On macOS, the native traffic-light buttons can overlap that remote UI.
                        // Prefer going fully borderless on macOS and rely on the decorationless
                        // move/resize handling.
                        #[cfg(target_os = "macos")]
                        let use_native_decorations = false;

                        #[cfg(not(target_os = "macos"))]
                        let use_native_decorations =
                            _toplevel.decoration_mode != Some(DecorationMode::Client);

                        #[cfg(not(target_os = "macos"))]
                        let title = _toplevel.title.clone().unwrap_or_else(|| "wprs".to_string());

                        // On macOS we draw a custom titlebar and keep the native title hidden, so
                        // avoid setting a non-empty native title string.
                        #[cfg(target_os = "macos")]
                        let title = String::new();

                        let mut attrs = Window::default_attributes().with_title(title);

                        if !use_native_decorations {
                            attrs = attrs.with_decorations(false);
                        }

                        #[cfg(target_os = "macos")]
                        let attrs = {
                            use winit::platform::macos::WindowAttributesExtMacOS as _;

                            if use_native_decorations {
                                // Keep native decorations/buttons, but avoid drawing behind the
                                // titlebar: remote apps may render their own custom titlebar
                                // inside the captured content, and drawing behind the titlebar
                                // causes the traffic-light buttons to overlap remote UI.
                                attrs.with_title_hidden(true)
                            } else {
                                attrs
                            }
                        };

                        attrs
                    } else {
                        Window::default_attributes()
                            .with_decorations(false)
                            .with_resizable(false)
                            .with_window_level(WindowLevel::AlwaysOnTop)
                    };

                    // We don't reserve a separate titlebar region: on macOS the titlebar is
                    // transparent and the remote content can be visible behind it.

                    if let Some(BufferAssignment::New(buf)) = &state.buffer {
                        let w = buf.metadata.width.max(1) as u32;
                        let h = buf.metadata.height.max(1) as u32;
                        let server_scale = state.buffer_scale.max(1) as f64;

                        if !self.warned_low_buffer_scale_on_hidpi {
                            let local_scale = event_loop
                                .primary_monitor()
                                .map(|m| m.scale_factor().round() as i32)
                                .unwrap_or(1)
                                .max(1);
                            if local_scale >= 2 && server_scale < 2.0 {
                                let server_suggested_scale =
                                    self.server_display_config.as_ref().map(|cfg| cfg.scale_factor);
                                warn!(
                                    "HiDPI display detected (scale_factor={local_scale}) but server sent buffer_scale={} (server DisplayConfig.scale_factor={server_suggested_scale:?}); rendering may be blurry. If this is a capture backend, increase server DPI/scale (e.g. wprsd display_dpi). If this is an app-hosting backend, ensure output scale is being advertised correctly (winit-wgpu: min_output_scale_factor={}).",
                                    state.buffer_scale.max(1),
                                    self.min_output_scale_factor
                                );
                                self.warned_low_buffer_scale_on_hidpi = true;
                            }
                        }
                        let logical_w = (f64::from(w) / server_scale).max(1.0);
                        let logical_h = (f64::from(h) / server_scale).max(1.0);
                        // Client-side scaling knob: magnify/shrink the window in logical units.
                        attrs = attrs.with_inner_size(LogicalSize::new(
                            logical_w * self.ui_scale(),
                            logical_h * self.ui_scale(),
                        ));

                        info!(
                            "creating window: surface={surface_id:?} kind={} buffer_px=({w}x{h}) buffer_scale={} ui_scale_factor={}",
                            if toplevel.is_some() {
                                "toplevel"
                            } else {
                                "popup"
                            },
                            state.buffer_scale,
                            self.ui_scale_factor
                        );
                    } else if let Some(popup) = popup {
                        attrs = attrs.with_inner_size(LogicalSize::new(
                            (popup.positioner.width.max(1) as f64) * self.ui_scale(),
                            (popup.positioner.height.max(1) as f64) * self.ui_scale(),
                        ));

                        if let Some(pos) = self.compute_popup_position(popup) {
                            attrs = attrs.with_position(pos);
                        }

                        info!(
                            "creating window: surface={surface_id:?} kind=popup positioner_px=({}x{}) ui_scale_factor={}",
                            popup.positioner.width, popup.positioner.height, self.ui_scale_factor
                        );
                    }

                    let window = Arc::new(event_loop.create_window(attrs).location(loc!())?);
                    let renderer =
                        WindowRenderer::new(&self.shared, window.clone()).location(loc!())?;
                    self.surface_by_window.insert(window.id(), surface_id);
                    if let Some(toplevel) = toplevel {
                        self.window_has_decorations.insert(
                            window.id(),
                            toplevel.decoration_mode != Some(DecorationMode::Client),
                        );
                    }
                    if let Ok(pos) = window.inner_position() {
                        self.last_window_inner_pos.insert(window.id(), pos);
                    }
                    self.windows.insert(surface_id, renderer);

                    // Start with a visible cursor even before the server sends its first cursor
                    // update; some compositors/apps only update the cursor after the first motion.
                    self.apply_cursor_for_surface(surface_id);
                    self.cursor_dirty = false;

                    if let Some(popup) = popup {
                        if let Some(pos) = self.compute_popup_position(popup) {
                            window.set_outer_position(pos);
                        }
                    }

                    // Send an initial configure so apps can begin drawing.
                    if toplevel.is_some() {
                        self.send_configure_for_surface(surface_id);
                    }
                }

                // Keep popup windows in sync with their parent.
                if let Some(popup) = popup {
                    self.update_popup_position(surface_id, popup);
                }

                // Apply buffer if present.
                if let Some(BufferAssignment::New(mut buf)) = state.buffer.take() {
                    if buf.data.is_external() {
                        if let Some(cache) = self.buffer_cache.take() {
                            buf.data = BufferData::Uncompressed(cache);
                        }
                    }
                    let filtered = match buf.data {
                        BufferData::Uncompressed(data) => data.0,
                        BufferData::Compressed(_) => {
                            warn!(
                                "Received buffer commit with inline Compressed data; skipping frame for {surface_id:?}"
                            );
                            return Ok(());
                        },
                        BufferData::External => {
                            if self.surfaces_with_frame.contains(&surface_id) {
                                warn!(
                                    "Received buffer commit with External data (no cached RawBuffer); skipping frame for {surface_id:?}"
                                );
                            } else {
                                debug!(
                                    "Received initial External buffer commit without a cached RawBuffer; waiting for first frame for {surface_id:?}"
                                );
                            }
                            return Ok(());
                        },
                    };
                    // Unfiltering can be expensive on non-SIMD platforms; do it
                    // off the winit/UI thread to keep the window responsive.
                    self.schedule_decode(surface_id, buf.metadata, filtered);
                }
                Ok(())
            },
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.outputs_sent {
            return;
        }
        self.outputs_sent = true;

        self.maybe_send_keymap();

        self.refresh_outputs(event_loop, true);
        self.last_outputs_refresh = Instant::now();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let interval = std::time::Duration::from_secs(2);
        if self.last_outputs_refresh.elapsed() < interval {
            return;
        }
        self.last_outputs_refresh = Instant::now();
        self.refresh_outputs(event_loop, false);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::ServerMessage(msg) => {
                self.handle_server_message(event_loop, msg)
                    .log_and_ignore(loc!());
            },
            UserEvent::DecodedFrame(frame) => {
                if let Some(renderer) = self.windows.get_mut(&frame.surface_id) {
                    renderer.update_texture_from_padded_bgra(
                        &self.shared,
                        &frame.metadata,
                        frame.padded_row_bytes,
                        &frame.data,
                    );
                    renderer.window.request_redraw();
                    self.surfaces_with_frame.insert(frame.surface_id);
                    return;
                }

                let Some(client) = self.cursor_surface_clients.get(&frame.surface_id).copied()
                else {
                    return;
                };
                let Some(cursor_frame) =
                    Self::bgra_padded_to_rgba(&frame.metadata, frame.padded_row_bytes, &frame.data)
                else {
                    return;
                };
                self.cursor_frames.insert(
                    ClientSurfaceKey {
                        client,
                        surface: frame.surface_id,
                    },
                    cursor_frame,
                );
            },
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let surface_id = self.surface_by_window.get(&window_id).copied();
        if surface_id.is_none() {
            return;
        }
        let surface_id = surface_id.unwrap();

        if let Some(renderer) = self.windows.get_mut(&surface_id) {
            match &event {
                WindowEvent::Resized(size) => {
                    renderer.resize(&self.shared, *size);
                    info!("window resized: surface={surface_id:?} size={size:?}");
                    if !self.popup_state_by_surface.contains_key(&surface_id) {
                        self.send_configure_for_surface(surface_id);
                    }
                    self.update_popups_for_parent(surface_id);
                },
                WindowEvent::ScaleFactorChanged { .. } => {
                    info!("window scale factor changed: surface={surface_id:?}");
                    let size = renderer.window.inner_size();
                    renderer.resize(&self.shared, size);
                    if !self.popup_state_by_surface.contains_key(&surface_id) {
                        self.send_configure_for_surface(surface_id);
                    }
                    self.update_popups_for_parent(surface_id);
                },
                WindowEvent::Moved(_) => {
                    if let Ok(pos) = renderer.window.inner_position() {
                        self.last_window_inner_pos.insert(window_id, pos);
                    }
                    if let Ok(pos) = renderer.window.outer_position() {
                        info!("window moved: surface={surface_id:?} outer_pos={pos:?}");
                    } else {
                        info!("window moved: surface={surface_id:?}");
                    }
                    self.update_popups_for_parent(surface_id);
                },
                WindowEvent::RedrawRequested => {
                    renderer.render(&self.shared).log_and_ignore(loc!());
                },
                WindowEvent::CloseRequested => {
                    info!("window close requested: surface={surface_id:?}");
                    if !self.popup_state_by_surface.contains_key(&surface_id) {
                        self.serializer
                            .writer()
                            .send(SendType::Object(proto::Event::Toplevel(
                                ToplevelEvent::Close(ToplevelClose { surface_id }),
                            )));
                    }
                    self.windows.remove(&surface_id);
                    self.surface_by_window.remove(&window_id);
                    self.window_has_decorations.remove(&window_id);
                    self.window_cursor_overridden.remove(&window_id);
                    self.last_window_cursor_pos_physical.remove(&window_id);
                    if self.focused_window == Some(window_id) {
                        self.focused_window = None;
                        self.set_keyboard_focus(None);
                    }
                },
                _ => {},
            }
        }

        match event {
            WindowEvent::Focused(true) => {
                self.focused_window = Some(window_id);
                let target = self
                    .popup_state_by_surface
                    .get(&surface_id)
                    .and_then(|popup| popup.grab_requested.then_some(surface_id))
                    .unwrap_or_else(|| self.keyboard_focus_target_for(surface_id));
                info!("window focused: surface={surface_id:?} keyboard_target={target:?}");
                self.set_keyboard_focus(Some(target));
            },
            WindowEvent::Focused(false) => {
                if self.focused_window == Some(window_id) {
                    self.focused_window = None;
                    info!("window unfocused: surface={surface_id:?}");
                    self.set_keyboard_focus(None);
                }
            },
            WindowEvent::ModifiersChanged(modifiers) => {
                self.local_modifiers = modifiers.state();
                self.send_modifiers(modifiers.state());
            },
            WindowEvent::KeyboardInput { event, .. } => {
                let winit::keyboard::PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let Some(linux_keycode) = Self::linux_keycode_from_winit(code) else {
                    debug!("unmapped keycode {code:?}");
                    return;
                };

                let state = match (event.state, event.repeat) {
                    (ElementState::Pressed, true) => KeyState::Repeated,
                    (ElementState::Pressed, false) => KeyState::Pressed,
                    (ElementState::Released, _) => KeyState::Released,
                };

                match state {
                    KeyState::Pressed | KeyState::Repeated => {
                        self.pressed_keycodes.insert(linux_keycode);
                    },
                    KeyState::Released => {
                        self.pressed_keycodes.remove(&linux_keycode);
                    },
                }
                self.send_key(linux_keycode, state);
            },
            WindowEvent::CursorMoved { position, .. } => {
                let Some(window) = self
                    .windows
                    .get(&surface_id)
                    .map(|renderer| renderer.window.clone())
                else {
                    return;
                };

                self.last_window_cursor_pos_physical.insert(window_id, position);
                let window_pos = coords::winit::physical_to_window_logical(window.as_ref(), position);
                self.last_window_cursor_pos.insert(window_id, window_pos);

                self.update_decorationless_cursor(surface_id, window_id, window.as_ref(), position);

                let pos = coords::winit::window_logical_to_remote_logical(
                    UiScaleFactor(self.ui_scale_factor),
                    window_pos,
                );
                self.last_cursor_pos.insert(window_id, pos);
                self.pointer_surface = Some(surface_id);

                // Ensure the server has pointer focus before motion/press; otherwise smithay will
                // drop motion and clicks land at (0,0).
                if self.pointer_inside.insert(window_id) {
                    let serial = self.next_serial();
                    self.send_pointer_event(surface_id, pos, PointerEventKind::Enter { serial });
                }
                self.send_pointer_event(surface_id, pos, PointerEventKind::Motion);

                if self.cursor_dirty {
                    self.apply_cursor_for_surface(surface_id);
                    self.cursor_dirty = false;
                }
            },
            WindowEvent::CursorEntered { .. } => {
                self.pointer_surface = Some(surface_id);

                if self.pointer_inside.insert(window_id) {
                    let serial = self.next_serial();
                    let pos = self.cursor_pos_for(window_id);
                    self.send_pointer_event(surface_id, pos, PointerEventKind::Enter { serial });
                }

                if self.cursor_dirty {
                    self.apply_cursor_for_surface(surface_id);
                    self.cursor_dirty = false;
                }
            },
            WindowEvent::CursorLeft { .. } => {
                let pos = self.cursor_pos_for(window_id);
                let serial = self.next_serial();
                self.send_pointer_event(surface_id, pos, PointerEventKind::Leave { serial });
                self.pointer_inside.remove(&window_id);
                if self.pointer_surface == Some(surface_id) {
                    self.pointer_surface = None;
                }
            },
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(renderer) = self.windows.get(&surface_id) else {
                    return;
                };

                if !self.window_has_decorations(window_id) {
                    // Match winit's `custom_decorations` example: right click shows the system
                    // window menu, if supported by the platform.
                    if state == ElementState::Pressed && button == MouseButton::Right {
                        // winit's Wayland implementation expects coordinates in *window-local
                        // logical* space (it converts incoming `Position` to logical internally).
                        //
                        // `MouseInput` doesn't include a position, so we use the last seen cursor
                        // location.
                        let Some(cursor_physical) =
                            self.last_window_cursor_pos_physical.get(&window_id).copied()
                        else {
                            return;
                        };

                        let menu_pos = coords::winit::window_menu_position(
                            renderer.window.as_ref(),
                            cursor_physical,
                        );
                        renderer.window.show_window_menu(menu_pos);
                        return;
                    }

                    // Edge resize takes priority.
                    if let Some(pos) = self.last_window_cursor_pos_physical.get(&window_id).copied()
                    {
                        if let Some(dir) =
                            self.hit_test_decorationless_resize(renderer.window.as_ref(), pos)
                        {
                            if state == ElementState::Pressed {
                                if let Err(err) = renderer.window.drag_resize_window(dir) {
                                    debug!("drag_resize_window failed: {err:?}");
                                }
                            }
                            return;
                        }
                    }

                    // Avoid stealing clicks from remote UI; require Alt/Option to move.
                    if state == ElementState::Pressed
                        && button == MouseButton::Left
                        && self.local_modifiers.alt_key()
                    {
                        if let Err(err) = renderer.window.drag_window() {
                            debug!("drag_window failed: {err:?}");
                        }
                        return;
                    }
                }

                let Some(button) = Self::linux_button_from_winit(button) else {
                    return;
                };
                let pos = self.cursor_pos_for(window_id);
                if self.pointer_inside.insert(window_id) {
                    let serial = self.next_serial();
                    self.send_pointer_event(surface_id, pos, PointerEventKind::Enter { serial });
                }
                let kind = match state {
                    ElementState::Pressed => PointerEventKind::Press {
                        button,
                        serial: self.next_serial(),
                    },
                    ElementState::Released => PointerEventKind::Release {
                        button,
                        serial: self.next_serial(),
                    },
                };
                self.send_pointer_event(surface_id, pos, kind);
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let (h_abs, v_abs, h_discrete, v_discrete) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        let v120_x = (x * 120.0) as i32;
                        let v120_y = (y * 120.0) as i32;
                        (f64::from(x) * 15.0, f64::from(y) * 15.0, v120_x, v120_y)
                    },
                    MouseScrollDelta::PixelDelta(pos) => {
                        let Some(renderer) = self.windows.get(&surface_id) else {
                            return;
                        };
                        let logical = pos.to_logical::<f64>(renderer.window.scale_factor());
                        // Trackpad (pixel) deltas in winit use the opposite sign convention from
                        // what most Wayland clients expect under natural scrolling.
                        let dx = -logical.x / self.ui_scale();
                        let dy = -logical.y / self.ui_scale();
                        (dx, dy, 0, 0)
                    },
                };
                let pos = self.cursor_pos_for(window_id);
                let source = match delta {
                    MouseScrollDelta::LineDelta(_, _) => Some(AxisSource::Wheel),
                    MouseScrollDelta::PixelDelta(_) => Some(AxisSource::Finger),
                };

                debug!(
                    "scroll: surface={surface_id:?} source={source:?} h_abs={h_abs:.2} v_abs={v_abs:.2} h_discrete={h_discrete} v_discrete={v_discrete}"
                );
                self.send_pointer_event(
                    surface_id,
                    pos,
                    PointerEventKind::Axis {
                        horizontal: AxisScroll {
                            absolute: h_abs,
                            discrete: h_discrete,
                            stop: false,
                        },
                        vertical: AxisScroll {
                            absolute: v_abs,
                            discrete: v_discrete,
                            stop: false,
                        },
                        source,
                    },
                );
            },
            WindowEvent::PinchGesture { delta, phase, .. } => {
                debug!("pinch: surface={surface_id:?} phase={phase:?} delta={delta:?}");
                let pos = self.cursor_pos_for(window_id);
                let state = self.pinch_state.entry(surface_id).or_default();
                let was_active = state.active_pinch || state.active_rotation;

                match phase {
                    winit::event::TouchPhase::Started => {
                        state.active_pinch = true;
                        if state.scale == 0.0 {
                            state.scale = 1.0;
                        }
                        if !was_active {
                            let serial = self.next_serial();
                            self.serializer.writer().send(SendType::Object(
                                proto::Event::PointerGesture(PointerGestureEvent::PinchBegin {
                                    surface_id,
                                    position: pos,
                                    serial,
                                    fingers: 2,
                                }),
                            ));
                        }
                    },
                    winit::event::TouchPhase::Moved => {
                        if delta.is_finite() {
                            // NSEvent magnification is additive; store an absolute scale for smithay.
                            state.scale = (state.scale + delta).clamp(0.1, 10.0);
                        }
                        self.serializer.writer().send(SendType::Object(
                            proto::Event::PointerGesture(PointerGestureEvent::PinchUpdate {
                                surface_id,
                                position: pos,
                                delta: (0.0, 0.0).into(),
                                scale: state.scale.max(0.1),
                                rotation: 0.0,
                            }),
                        ));
                    },
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled => {
                        state.active_pinch = false;
                        let cancelled = matches!(phase, winit::event::TouchPhase::Cancelled);
                        let is_active = state.active_pinch || state.active_rotation;
                        if !is_active {
                            self.pinch_state.remove(&surface_id);
                            let serial = self.next_serial();
                            self.serializer.writer().send(SendType::Object(
                                proto::Event::PointerGesture(PointerGestureEvent::PinchEnd {
                                    surface_id,
                                    position: pos,
                                    serial,
                                    cancelled,
                                }),
                            ));
                        }
                    },
                }
            },

            WindowEvent::RotationGesture { delta, phase, .. } => {
                debug!("rotation: surface={surface_id:?} phase={phase:?} delta_deg={delta}");
                let pos = self.cursor_pos_for(window_id);
                let state = self.pinch_state.entry(surface_id).or_default();
                let was_active = state.active_pinch || state.active_rotation;
                match phase {
                    winit::event::TouchPhase::Started => {
                        state.active_rotation = true;
                        if state.scale == 0.0 {
                            state.scale = 1.0;
                        }
                        if !was_active {
                            let serial = self.next_serial();
                            self.serializer.writer().send(SendType::Object(
                                proto::Event::PointerGesture(PointerGestureEvent::PinchBegin {
                                    surface_id,
                                    position: pos,
                                    serial,
                                    fingers: 2,
                                }),
                            ));
                        }
                    },
                    winit::event::TouchPhase::Moved => {
                        // winit uses CCW-positive, smithay uses clockwise-positive.
                        let rotation = -(delta as f64);
                        self.serializer.writer().send(SendType::Object(
                            proto::Event::PointerGesture(PointerGestureEvent::PinchUpdate {
                                surface_id,
                                position: pos,
                                delta: (0.0, 0.0).into(),
                                scale: state.scale.max(0.1),
                                rotation,
                            }),
                        ));
                    },
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled => {
                        state.active_rotation = false;
                        let cancelled = matches!(phase, winit::event::TouchPhase::Cancelled);
                        let is_active = state.active_pinch || state.active_rotation;
                        if !is_active {
                            self.pinch_state.remove(&surface_id);
                            let serial = self.next_serial();
                            self.serializer.writer().send(SendType::Object(
                                proto::Event::PointerGesture(PointerGestureEvent::PinchEnd {
                                    surface_id,
                                    position: pos,
                                    serial,
                                    cancelled,
                                }),
                            ));
                        }
                    },
                }
            },
            _ => {},
        }
    }
}

pub fn run(
    mut serializer: Serializer<proto::Event, Request>,
    options: WinitWgpuOptions,
) -> Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    let (decode_tx, decode_rx) = std::sync::mpsc::channel::<DecodeJob>();
    {
        let proxy = proxy.clone();
        thread::spawn(move || {
            while let Ok(job) = decode_rx.recv() {
                let (padded_row_bytes, padded) =
                    decode_filtered_to_padded_bgra(&job.metadata, &job.filtered);
                let _ = proxy.send_event(UserEvent::DecodedFrame(DecodedFrame {
                    surface_id: job.surface_id,
                    metadata: job.metadata,
                    padded_row_bytes,
                    data: padded,
                }));
            }
        });
    }

    // Forward serializer messages to the winit event loop.
    let reader = serializer.reader().location(loc!())?;
    let proxy_for_reader = proxy.clone();
    thread::spawn(move || {
        let mut loop_: CalloopEventLoop<()> = CalloopEventLoop::try_new().expect("calloop init");
        loop_
            .handle()
            .insert_source(reader, move |event, _metadata, _state| {
                if let CalloopChannelEvent::Msg(msg) = event {
                    proxy_for_reader
                        .send_event(UserEvent::ServerMessage(msg))
                        .ok();
                }
            })
            .expect("insert serializer reader");

        loop_.run(None, &mut (), |_| {}).ok();
    });

    // Init shared wgpu context.
    let instance = Arc::new(wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    }));

    let adapter = Arc::new(
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .location(loc!())?,
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("wprs_winit_wgpu_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .location(loc!())?;

    debug!("wgpu adapter: {:?}", adapter.get_info());

    let shared = WgpuShared {
        instance,
        adapter,
        device: Arc::new(device),
        queue: Arc::new(queue),
    };

        let mut app = App {
        shared,
        serializer,
        decode_tx,
        buffer_cache: None,
        windows: HashMap::new(),
        surface_by_window: HashMap::new(),
        outputs_sent: false,

        last_outputs_refresh: Instant::now(),
        last_outputs: Vec::new(),
        min_output_scale_factor: options.min_output_scale_factor.max(1),

        server_display_config: None,
        surface_scale_factor: HashMap::new(),

        keyboard_mode: options.keyboard_mode,
        xkb_keymap_sent: false,
        xkb_keymap_file: options.xkb_keymap_file,
        ui_scale_factor: options.ui_scale_factor,

        serial_counter: 1,
        focused_window: None,
        focused_surface: None,
        surfaces_with_frame: HashSet::new(),
        pressed_keycodes: HashSet::new(),
        last_window_cursor_pos: HashMap::new(),
        last_cursor_pos: HashMap::new(),
        last_window_cursor_pos_physical: HashMap::new(),
        local_modifiers: winit::keyboard::ModifiersState::default(),
        window_has_decorations: HashMap::new(),
        window_cursor_overridden: HashSet::new(),
        pointer_inside: HashSet::new(),
        pointer_surface: None,

        last_window_inner_pos: HashMap::new(),
        pinch_state: HashMap::new(),

        popup_state_by_surface: HashMap::new(),

        cursor_frames: HashMap::new(),
        cursor_surface_clients: HashMap::new(),

        current_cursor: Some(Cursor::from(CursorIcon::Default)),
        warned_cursor_names: HashSet::new(),
        cursor_dirty: true,

        warned_low_buffer_scale_on_hidpi: false,
    };

    event_loop.run_app(&mut app)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct WinitWgpuClientBackend {
    options: WinitWgpuOptions,
}

impl WinitWgpuClientBackend {
    pub fn new(config: crate::client::backend::ClientBackendConfig) -> Self {
        #[cfg(target_os = "macos")]
        let default_min_output_scale_factor = 2;

        #[cfg(not(target_os = "macos"))]
        let default_min_output_scale_factor = 1;

        let min_output_scale_factor = config
            .min_output_scale_factor
            .unwrap_or(default_min_output_scale_factor)
            .max(1);
        Self {
            options: WinitWgpuOptions {
                keyboard_mode: config.keyboard_mode,
                xkb_keymap_file: config.xkb_keymap_file,
                ui_scale_factor: config.ui_scale_factor,
                min_output_scale_factor,
            },
        }
    }
}

impl crate::client::backend::ClientBackend for WinitWgpuClientBackend {
    fn name(&self) -> &'static str {
        "winit-wgpu"
    }

    fn run(self: Box<Self>, serializer: Serializer<proto::Event, proto::Request>) -> Result<()> {
        run(serializer, self.options).location(loc!())
    }
}
