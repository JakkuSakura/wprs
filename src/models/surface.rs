use enum_as_inner::EnumAsInner;
use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

use crate::protocols::wprs::types::ClientId;
use crate::protocols::wprs::geometry::Point;
use crate::protocols::wprs::geometry::Rectangle;
use crate::protocols::wprs::geometry::Size;
use crate::protocols::wprs::tuple::Tuple2;
use crate::protocols::wprs::xdg_shell;
use crate::utils::vec4u8::Vec4u8s;

/// Stable surface identifier.
#[derive(Archive, Deserialize, Serialize, Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct WlSurfaceId(pub u64);

/// Stable subsurface identifier.
#[derive(Archive, Deserialize, Serialize, Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct SubSurfaceId(pub u64);

#[derive(Debug, Copy, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct ClientSurface {
    pub client: ClientId,
    pub surface: WlSurfaceId,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, EnumAsInner, Archive, Deserialize, Serialize)]
pub enum BufferFormat {
    Argb8888,
    Xrgb8888,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct BufferMetadata {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: BufferFormat,
}

impl BufferMetadata {
    pub fn pixel_bytes(&self) -> i32 {
        self.stride / self.width
    }

    pub fn len(&self) -> usize {
        (self.height * self.stride) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct UncompressedBufferData(pub Vec4u8s);

impl std::fmt::Debug for UncompressedBufferData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UncompressedBufferData")
            .field(&format_args!("Vec4u8s[{}]", self.0.len()))
            .finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, EnumAsInner, Archive, Deserialize, Serialize)]
pub enum BufferData {
    External,
    Uncompressed(UncompressedBufferData),
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct Buffer {
    pub metadata: BufferMetadata,
    pub data: BufferData,
}

#[derive(Debug, Clone, Eq, PartialEq, EnumAsInner, Archive, Deserialize, Serialize)]
pub enum BufferAssignment {
    New(Buffer),
    Removed,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct SubSurfaceState {
    pub parent: WlSurfaceId,
    pub location: Point<i32>,
    pub sync: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, EnumAsInner, Archive, Deserialize, Serialize)]
pub enum Role {
    Cursor(Point<i32>),
    SubSurface(SubSurfaceState),
    XdgToplevel(xdg_shell::XdgToplevelState),
    XdgPopup(xdg_shell::XdgPopupState),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum RectangleKind {
    Add,
    Subtract,
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct Region {
    pub rects: Vec<Tuple2<RectangleKind, Rectangle<i32>>>,
}

impl Default for Region {
    fn default() -> Self {
        Self { rects: Vec::new() }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum Transform {
    Normal,
    _90,
    _180,
    _270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct SubsurfacePosition {
    pub id: WlSurfaceId,
    pub position: Point<i32>,
}

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub struct SurfaceState {
    pub client: ClientId,
    pub id: WlSurfaceId,
    pub buffer: Option<BufferAssignment>,
    pub buffer_update: Option<BufferUpdate>,
    pub role: Option<Role>,
    pub buffer_scale: i32,
    pub buffer_transform: Option<Transform>,
    pub opaque_region: Option<Region>,
    pub input_region: Option<Region>,
    pub z_ordered_children: Vec<SubsurfacePosition>,
    pub damage: Option<Vec<Rectangle<i32>>>,
    pub output_ids: Vec<u32>,
    pub viewport_state: Option<ViewportState>,
    pub xdg_surface_state: Option<xdg_shell::XdgSurfaceState>,
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum BufferUpdate {
    Patch {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        stride: i32,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub struct ViewportState {
    pub src: Option<Rectangle<f64>>,
    pub dst: Option<Size<i32>>,
}
