use enum_as_inner::EnumAsInner;
use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;
use rkyv::rancor::Fallible;
use std::sync::Arc;

use crate::protocols::wprs::types::ClientId;
use crate::protocols::wprs::geometry::Point;
use crate::protocols::wprs::geometry::Rectangle;
use crate::protocols::wprs::geometry::Size;
use crate::protocols::wprs::tuple::Tuple2;
use crate::protocols::wprs::xdg_shell;
use crate::utils::vec4u8::Vec4u8;

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

#[derive(Clone, Eq, PartialEq)]
pub struct BufferPoolHandle(pub Arc<Vec<u8>>);

impl std::fmt::Debug for BufferPoolHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BufferPoolHandle")
            .field(&format_args!("bytes[{}]", self.0.len()))
            .finish()
    }
}

impl BufferPoolHandle {
    pub fn new(size: usize) -> Self {
        Self(Arc::new(vec![0; size]))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        Arc::get_mut(&mut self.0).map(Vec::as_mut_slice)
    }

    pub fn as_vec4u8s(&self) -> Option<&[Vec4u8]> {
        if self.len() % 4 != 0 {
            return None;
        }
        Some(bytemuck::cast_slice(self.as_slice()))
    }

    pub fn as_vec4u8s_mut(&mut self) -> Option<&mut [Vec4u8]> {
        if self.len() % 4 != 0 {
            return None;
        }
        self.as_mut_slice()
            .map(|slice| bytemuck::cast_slice_mut(slice))
    }
}

impl From<Vec<u8>> for BufferPoolHandle {
    fn from(value: Vec<u8>) -> Self {
        Self(Arc::new(value))
    }
}

impl From<Arc<Vec<u8>>> for BufferPoolHandle {
    fn from(value: Arc<Vec<u8>>) -> Self {
        Self(value)
    }
}

impl Archive for BufferPoolHandle {
    type Archived = rkyv::vec::ArchivedVec<u8>;
    type Resolver = rkyv::vec::VecResolver;

    fn resolve(
        &self,
        resolver: Self::Resolver,
        out: rkyv::Place<Self::Archived>,
    ) {
        self.0.as_ref().resolve(resolver, out);
    }
}

impl<S> Serialize<S> for BufferPoolHandle
where
    S: Fallible,
    Vec<u8>: Serialize<S>,
{
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

impl<D> Deserialize<BufferPoolHandle, D> for rkyv::vec::ArchivedVec<u8>
where
    D: Fallible,
    rkyv::vec::ArchivedVec<u8>: Deserialize<Vec<u8>, D>,
{
    fn deserialize(&self, deserializer: &mut D) -> Result<BufferPoolHandle, D::Error> {
        let data = rkyv::vec::ArchivedVec::<u8>::deserialize(self, deserializer)?;
        Ok(BufferPoolHandle(Arc::new(data)))
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct Bitmap {
    pub metadata: BufferMetadata,
    pub data: BufferPoolHandle,
}

impl Bitmap {
    pub fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    pub fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        self.data.as_mut_slice()
    }

    pub fn pixels(&self) -> Option<&[Vec4u8]> {
        self.data.as_vec4u8s()
    }

    pub fn pixels_mut(&mut self) -> Option<&mut [Vec4u8]> {
        self.data.as_vec4u8s_mut()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, EnumAsInner, Archive, Deserialize, Serialize)]
pub enum BitmapAssignment {
    New(Bitmap),
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
    pub bitmap: Option<BitmapAssignment>,
    pub bitmap_update: Option<BitmapUpdate>,
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
pub enum BitmapUpdate {
    Patch {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        stride: i32,
        data: BufferPoolHandle,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub struct ViewportState {
    pub src: Option<Rectangle<f64>>,
    pub dst: Option<Size<i32>>,
}
