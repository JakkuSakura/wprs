use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

/// Client-reported CPU features.
///
/// This is intentionally conservative: it is best-effort metadata that the server can
/// use to pick a sensible default.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct CpuFeatures {
    pub avx2: bool,
    pub neon: bool,
}

impl Default for CpuFeatures {
    fn default() -> Self {
        Self {
            avx2: false,
            neon: false,
        }
    }
}

/// Client-reported GPU / acceleration capabilities.
///
/// This is not meant to be a perfect hardware database; it's a set of hints for
/// transport policy.
#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct GpuFeatures {
    /// Whether the client can efficiently upload textures and do scaling/compositing on GPU.
    pub has_gpu_rendering: bool,

    /// Whether the client supports decoding typical video codecs via hardware acceleration.
    pub has_hw_video_decode: bool,

    /// Best-effort list of hardware decode codecs supported by the client.
    ///
    /// Example values: "h264", "hevc", "vp9", "av1".
    pub hw_decode_codecs: Vec<String>,
}

impl Default for GpuFeatures {
    fn default() -> Self {
        Self {
            has_gpu_rendering: false,
            has_hw_video_decode: false,
            hw_decode_codecs: Vec::new(),
        }
    }
}
