use crate::prelude::*;
use crate::protocols::wprs::RawBufferKind;
use crate::protocols::wprs::RecvType;
use crate::protocols::wprs::Request;
use crate::protocols::wprs::wayland::BufferAssignment;
use crate::protocols::wprs::wayland::BufferData;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::protocols::wprs::wayland::UncompressedBufferData;
use crate::utils::buffer_pointer::BufferPointer;
use crate::utils::filtering;

/// Client-side synchronizer for pairing `RawBuffer` frames with `Surface(Commit)` messages.
///
/// The current WPRS wire format externalizes frame bytes as `RawBuffer` messages and uses
/// `BufferData::External` in the surface commit as a placeholder. This helper keeps the
/// association logic out of presentation backends.
#[derive(Default)]
pub struct ClientSync {
    buffer_cache: std::collections::HashMap<crate::protocols::wprs::wayland::WlSurfaceId, UncompressedBufferData>,
    legacy_last_buffer: Option<UncompressedBufferData>,
    #[cfg(feature = "video-h264")]
    h264_decoder: std::collections::HashMap<
        crate::protocols::wprs::wayland::WlSurfaceId,
        crate::protocols::video::h264::H264Decoder,
    >,
}

impl ClientSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Transforms incoming server messages:
    /// - Consumes `RecvType::RawBuffer` frames, decoding/filtering into an in-memory buffer.
    /// - Rewrites `Surface(Commit)` messages to inline the decoded buffer when `External`.
    ///
    /// Returns `None` for messages that are fully handled by the synchronizer.
    pub fn handle_message(&mut self, msg: RecvType<Request>) -> Result<Option<RecvType<Request>>> {
        match msg {
            RecvType::RawBuffer(msg) => {
                let surface = msg.header.surface;
                match msg.header.kind {
                    RawBufferKind::FilteredBgra => {
                        let data = UncompressedBufferData(msg.bytes.into());
                        if let Some(surface) = surface {
                            self.buffer_cache.insert(surface, data);
                        } else {
                            self.legacy_last_buffer = Some(data);
                        }
                    }
                    #[cfg(feature = "video-h264")]
                    RawBufferKind::H264 => {
                        let Some(surface) = surface else {
                            warn!("received H264 buffer without surface id; ignoring");
                            return Ok(None);
                        };

                        let decoder = self
                            .h264_decoder
                            .entry(surface)
                            .or_insert_with(|| {
                                crate::protocols::video::h264::H264Decoder::new()
                                    .expect("H264Decoder init")
                            });

                        let decoded = decoder.decode(&msg.bytes).location(loc!())?;
                        let Some(decoded) = decoded else {
                            return Ok(None);
                        };
                        let ptr = decoded.bgra.as_ptr();
                        let data = unsafe { BufferPointer::new(&ptr, decoded.bgra.len()) };
                        let filtered = filtering::filter_to_vec4u8s(data);
                        let data = UncompressedBufferData(filtered);
                        self.buffer_cache.insert(surface, data);
                    }
                    #[cfg(not(feature = "video-h264"))]
                    RawBufferKind::H264 => {
                        warn!("received H264 buffer without video-h264 support");
                    }
                    RawBufferKind::Png => {
                        let (_w, _h, bgra) =
                            crate::protocols::wprs::transport::decode_png_to_bgra(&msg.bytes)
                                .location(loc!())?;
                        let ptr = bgra.as_ptr();
                        let data = unsafe { BufferPointer::new(&ptr, bgra.len()) };
                        let filtered = filtering::filter_to_vec4u8s(data);
                        let data = UncompressedBufferData(filtered);
                        if let Some(surface) = surface {
                            self.buffer_cache.insert(surface, data);
                        } else {
                            self.legacy_last_buffer = Some(data);
                        }
                    }
                    RawBufferKind::Jpeg => {
                        let (_w, _h, bgra) =
                            crate::protocols::wprs::transport::decode_jpeg_to_bgra(&msg.bytes)
                                .location(loc!())?;
                        let ptr = bgra.as_ptr();
                        let data = unsafe { BufferPointer::new(&ptr, bgra.len()) };
                        let filtered = filtering::filter_to_vec4u8s(data);
                        let data = UncompressedBufferData(filtered);
                        if let Some(surface) = surface {
                            self.buffer_cache.insert(surface, data);
                        } else {
                            self.legacy_last_buffer = Some(data);
                        }
                    }
                }

                Ok(None)
            }
            RecvType::Object(Request::Surface(mut surface)) => {
                match &surface.payload {
                    SurfaceRequestPayload::Destroyed => {
                        self.buffer_cache.remove(&surface.surface);
                        #[cfg(feature = "video-h264")]
                        self.h264_decoder.remove(&surface.surface);
                        return Ok(Some(RecvType::Object(Request::Surface(surface))));
                    }
                    SurfaceRequestPayload::Commit(_) => {}
                }

                let SurfaceRequestPayload::Commit(mut state) = surface.payload else {
                    unreachable!()
                };

                if let Some(BufferAssignment::New(mut buf)) = state.buffer.take() {
                    if buf.data.is_external() {
                        if let Some(cache) = self.buffer_cache.remove(&surface.surface) {
                            buf.data = BufferData::Uncompressed(cache);
                        } else if let Some(cache) = self.legacy_last_buffer.take() {
                            buf.data = BufferData::Uncompressed(cache);
                        }
                    }
                    state.buffer = Some(BufferAssignment::New(buf));
                }

                surface.payload = SurfaceRequestPayload::Commit(state);
                Ok(Some(RecvType::Object(Request::Surface(surface))))
            }
            other => Ok(Some(other)),
        }
    }
}
