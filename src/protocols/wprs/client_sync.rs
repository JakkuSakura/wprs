use crate::prelude::*;
use crate::protocols::wprs::raw_buffer::RawBufferKind;
use crate::protocols::wprs::serializer::RecvType;
use crate::protocols::wprs::types::Request;
use crate::protocols::wprs::wayland::BitmapAssignment;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::protocols::wprs::wayland::BufferPoolHandle;
use crate::utils::filtering;

/// Client-side synchronizer for pairing `RawBuffer` frames with `Surface(Commit)` messages.
///
/// The current WPRS wire format externalizes frame bytes as `RawBuffer` messages and may
/// carry empty buffer payloads in surface commits as placeholders. This helper keeps the
/// association logic out of presentation backends.
#[derive(Default)]
pub struct ClientSync {
    buffer_cache: std::collections::HashMap<crate::protocols::wprs::wayland::WlSurfaceId, BufferPoolHandle>,
    legacy_last_buffer: Option<BufferPoolHandle>,
    #[cfg(feature = "video-h264")]
    h264_decoder: std::collections::HashMap<
        crate::protocols::wprs::wayland::WlSurfaceId,
        crate::protocols::video::h264::H264Decoder,
    >,
    #[cfg(feature = "video-h264")]
    h264_disabled: std::collections::HashSet<crate::protocols::wprs::wayland::WlSurfaceId>,
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
                        let filtered = crate::utils::vec4u8::Vec4u8s::from(msg.bytes);
                        let mut bgra = vec![0u8; filtered.len() * 4];
                        filtering::unfilter(&filtered, &mut bgra);
                        let data = BufferPoolHandle::from(bgra);
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

                        if self.h264_disabled.contains(&surface) {
                            return Ok(None);
                        }

                        let decoder = match self.h264_decoder.entry(surface) {
                            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                match crate::protocols::video::h264::H264Decoder::new() {
                                    Ok(decoder) => entry.insert(decoder),
                                    Err(err) => {
                                        warn!("H264 decoder init failed: {err:?}");
                                        self.h264_disabled.insert(surface);
                                        return Ok(None);
                                    }
                                }
                            }
                        };

                        let decoded = match decoder.decode(&msg.bytes) {
                            Ok(decoded) => decoded,
                            Err(err) => {
                                warn!("H264 decode failed; disabling for surface {surface:?}: {err:?}");
                                self.h264_decoder.remove(&surface);
                                self.h264_disabled.insert(surface);
                                return Ok(None);
                            }
                        };
                        let Some(decoded) = decoded else {
                            return Ok(None);
                        };
                        self.buffer_cache
                            .insert(surface, BufferPoolHandle::from(decoded.bgra));
                    }
                    #[cfg(not(feature = "video-h264"))]
                    RawBufferKind::H264 => {
                        warn!("received H264 buffer without video-h264 support");
                    }
                    RawBufferKind::Png => {
                        let (_w, _h, bgra) =
                            crate::protocols::wprs::transport::decode_png_to_bgra(&msg.bytes)
                                .location(loc!())?;
                        let data = BufferPoolHandle::from(bgra);
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
                        let data = BufferPoolHandle::from(bgra);
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
                        {
                            self.h264_decoder.remove(&surface.surface);
                            self.h264_disabled.remove(&surface.surface);
                        }
                        return Ok(Some(RecvType::Object(Request::Surface(surface))));
                    }
                    SurfaceRequestPayload::Commit(_) => {}
                }

                let SurfaceRequestPayload::Commit(mut state) = surface.payload else {
                    unreachable!()
                };

                if let Some(BitmapAssignment::New(mut buf)) = state.bitmap.take() {
                    if buf.data.len() != buf.metadata.len() {
                        if let Some(cache) = self.buffer_cache.remove(&surface.surface) {
                            buf.data = cache;
                        } else if let Some(cache) = self.legacy_last_buffer.take() {
                            buf.data = cache;
                        }
                    }
                    state.bitmap = Some(BitmapAssignment::New(buf));
                }

                surface.payload = SurfaceRequestPayload::Commit(state);
                Ok(Some(RecvType::Object(Request::Surface(surface))))
            }
            other => Ok(Some(other)),
        }
    }
}
