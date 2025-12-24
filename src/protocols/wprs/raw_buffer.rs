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

use std::io::Read;
use std::io::Write;
use std::num::NonZeroUsize;

use fallible_iterator::IteratorExt;

use crate::prelude::*;
use crate::utils::sharding_compression::CompressedShards;
use crate::utils::sharding_compression::ShardingDecompressor;

use super::framing::Framed;
use super::wayland;

/// Payload sent over the WPRS transport plane as a `MessageType::RawBuffer` frame.
///
/// This is intentionally *not* rkyv-serialized. It uses the custom `Framed`
/// encoding implemented by `CompressedShards` for efficient streaming.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
pub enum RawBufferKind {
    /// Filtered pixel bytes (Vec4u8s / SOA filter output).
    FilteredBgra = 1,
    /// H.264 bitstream bytes.
    H264 = 2,
    /// PNG image bytes.
    Png = 3,
    /// JPEG image bytes.
    Jpeg = 4,
}

impl Framed for RawBufferKind {
    fn framed_write<W: Write>(&self, stream: &mut W) -> Result<()> {
        (*self as u8).framed_write(stream)
    }

    fn framed_read<R: Read>(stream: &mut R) -> Result<Self> {
        match u8::framed_read(stream).location(loc!())? {
            1 => Ok(Self::FilteredBgra),
            2 => Ok(Self::H264),
            3 => Ok(Self::Png),
            4 => Ok(Self::Jpeg),
            other => bail!(Error::InvalidArgument(format!(
                "invalid RawBufferKind {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RawBufferHeader {
    pub version: u8,
    pub kind: RawBufferKind,
    pub surface: Option<wayland::WlSurfaceId>,
}

impl RawBufferHeader {
    pub const V1: u8 = 1;
    pub const V2: u8 = 2;
}

impl Framed for RawBufferHeader {
    fn framed_write<W: Write>(&self, stream: &mut W) -> Result<()> {
        self.version.framed_write(stream).location(loc!())?;
        self.kind.framed_write(stream).location(loc!())?;
        if self.version >= Self::V2 {
            let surface = self
                .surface
                .ok_or_else(|| {
                    Error::InvalidArgument("RawBufferHeader v2 requires surface".to_string())
                })
                .location(loc!())?;
            surface.framed_write(stream).location(loc!())?;
        }
        Ok(())
    }

    fn framed_read<R: Read>(stream: &mut R) -> Result<Self> {
        let version = u8::framed_read(stream).location(loc!())?;
        let kind = RawBufferKind::framed_read(stream).location(loc!())?;
        let surface = if version >= Self::V2 {
            Some(wayland::WlSurfaceId::framed_read(stream).location(loc!())?)
        } else {
            None
        };
        Ok(Self {
            version,
            kind,
            surface,
        })
    }
}

impl Framed for wayland::WlSurfaceId {
    fn framed_write<W: Write>(&self, stream: &mut W) -> Result<()> {
        stream.write_all(&self.0.to_be_bytes()).location(loc!())
    }

    fn framed_read<R: Read>(stream: &mut R) -> Result<Self> {
        let mut buf = [0u8; std::mem::size_of::<u64>()];
        stream.read_exact(&mut buf).location(loc!())?;
        Ok(Self(u64::from_be_bytes(buf)))
    }
}

#[derive(Clone)]
pub struct RawBufferPayload {
    pub surface: wayland::WlSurfaceId,
    pub kind: RawBufferKind,
    pub shards: CompressedShards,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RawBufferMessage {
    pub header: RawBufferHeader,
    pub bytes: Vec<u8>,
}

#[allow(dead_code)]
pub(crate) fn extract_single_uncompressed_shard(
    shards: CompressedShards,
) -> std::result::Result<Vec<u8>, CompressedShards> {
    if shards.shards.len() != 1 {
        return Err(shards);
    }

    let shard = &shards.shards[0];
    if shard.idx != 0 || shard.compression || shard.uncompressed_size != shard.data.len() {
        return Err(shards);
    }

    let mut shards = shards;
    let shard = shards.shards.pop().expect("checked len == 1");
    Ok(shard.data)
}

#[allow(dead_code)]
pub(crate) fn decompress_shards_to_owned(shards: CompressedShards) -> Result<Vec<u8>> {
    if shards.is_empty() {
        return Ok(Vec::new());
    }

    let indices = shards.indices();
    let uncompressed_size = shards.uncompressed_size();
    let shards_iter = shards
        .shards
        .into_iter()
        .map(Ok::<_, crate::error::Error>)
        .transpose_into_fallible();

    // Avoid spawning lots of decompressor threads; in-process transport is
    // already low-latency.
    let mut decompressor =
        ShardingDecompressor::new(NonZeroUsize::new(2).unwrap()).location(loc!())?;
    decompressor
        .decompress_to_owned(&indices, uncompressed_size, shards_iter)
        .location(loc!())
}
