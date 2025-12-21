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

/// User preferences for transport tuning.
#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct TransportPreferences {
    /// Optional target bitrate hint, in kilobits/sec.
    pub target_bitrate_kbps: Option<u32>,
    /// Optional max RTT hint, in milliseconds.
    pub max_rtt_ms: Option<u32>,

    /// Relative preference weights (0..=100).
    ///
    /// These are hints used by the transport policy when trade-offs are needed.
    pub latency_weight: u8,
    pub bandwidth_weight: u8,
    pub cpu_weight: u8,
    pub clarity_weight: u8,
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

impl Default for TransportPreferences {
    fn default() -> Self {
        Self {
            target_bitrate_kbps: None,
            max_rtt_ms: None,
            latency_weight: 25,
            bandwidth_weight: 25,
            cpu_weight: 25,
            clarity_weight: 25,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum TransportCodec {
    /// Frame payloads are sharded and optionally zstd-compressed.
    ///
    /// This is the current on-wire representation (see `CompressedShard::compression`).
    ShardedZstd { level: i32 },
    /// Frame payloads are sharded but never compressed (bandwidth-heavy, CPU-light).
    ShardedRaw,
    /// Frame payloads are sharded and LZ4-compressed (low CPU, moderate compression).
    ShardedLz4,
}

impl Default for TransportCodec {
    fn default() -> Self {
        Self::ShardedZstd { level: 1 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Archive, Deserialize, Serialize)]
pub struct BufferPatchConfig {
    pub enabled: bool,
    /// Tile size used for dirty detection (in pixels).
    pub tile_px: u32,
    /// If more than this fraction of tiles changed, send a full frame instead of a patch.
    pub full_frame_threshold: f32,
}

impl Default for BufferPatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tile_px: 64,
            full_frame_threshold: 0.6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub struct TransportConfig {
    pub codec: TransportCodec,
    pub buffer_patches: BufferPatchConfig,
    /// Optional server-side frame rate cap.
    pub max_fps: Option<u32>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            codec: TransportCodec::default(),
            buffer_patches: BufferPatchConfig::default(),
            max_fps: None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct ClientHello {
    pub supported_codecs: Vec<TransportCodec>,
    pub supports_buffer_patches: bool,
    pub cpu: CpuFeatures,
    pub gpu: GpuFeatures,
    pub preferences: TransportPreferences,
}

impl Default for ClientHello {
    fn default() -> Self {
        Self {
            supported_codecs: vec![
                TransportCodec::default(),
                TransportCodec::ShardedLz4,
                TransportCodec::ShardedRaw,
            ],
            supports_buffer_patches: false,
            cpu: CpuFeatures::default(),
            gpu: GpuFeatures::default(),
            preferences: TransportPreferences::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct Ping {
    pub seq: u64,
    /// Milliseconds since UNIX_EPOCH.
    pub sent_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct Pong {
    pub seq: u64,
    /// Milliseconds since UNIX_EPOCH.
    pub sent_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct TransportStats {
    /// Best-effort round-trip time observed by the client.
    pub rtt_ms: u32,

    /// Best-effort transport goodput as observed by the client.
    pub rx_kbps: u32,
    pub tx_kbps: u32,

    /// Best-effort mean decode time per frame (milliseconds).
    pub decode_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub enum TransportRequest {
    Config(TransportConfig),
    Pong(Pong),
}

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub enum TransportEvent {
    ClientHello(ClientHello),
    Ping(Ping),
    Stats(TransportStats),
}

