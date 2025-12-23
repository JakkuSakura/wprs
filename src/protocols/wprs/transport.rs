use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

use anyhow::bail;
use anyhow::ensure;

use crate::prelude::*;

use super::wayland::WlSurfaceId;

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
    pub usage_goal: Option<UsageGoal>,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum UsageGoal {
    /// Maximize bandwidth utilization and minimize latency; allow drops and lossiness.
    Gaming,
    /// Maximize clarity while tolerating drops; avoid lossy codecs when possible.
    Office,
    /// Maximize clarity with compression; avoid drops and lossy codecs when possible.
    Media,
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
            usage_goal: None,
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
    /// Frame payloads are sharded (split into independently-transferred chunks)
    /// and optionally zstd-compressed.
    ///
    /// This is the current on-wire representation (see `CompressedShard::compression`).
    ShardedZstd { level: i32 },
    /// Frame payloads are sharded but never compressed (bandwidth-heavy, CPU-light).
    ShardedRaw,
    /// Frame payloads are sharded and LZ4-compressed (low CPU, moderate compression).
    ShardedLz4,
    /// Frame payloads are H.264 bitstreams (requires the `video-h264` feature).
    H264,
    /// Frame payloads are PNG image bytes.
    Png,
    /// Frame payloads are JPEG image bytes.
    Jpeg,
}

impl Default for TransportCodec {
    fn default() -> Self {
        Self::ShardedZstd { level: 1 }
    }
}

pub fn encode_png_from_bgra(
    width: u32,
    height: u32,
    stride_bytes: usize,
    bgra: &[u8],
) -> Result<Vec<u8>> {
    let height_usize = height as usize;
    let width_usize = width as usize;
    ensure!(width_usize != 0 && height_usize != 0, "invalid image size");
    ensure!(
        stride_bytes >= width_usize * 4,
        "stride too small: stride={stride_bytes} width={width}"
    );
    ensure!(
        bgra.len() >= stride_bytes.saturating_mul(height_usize),
        "bgra buffer too small: len={} expected_at_least={}",
        bgra.len(),
        stride_bytes.saturating_mul(height_usize)
    );

    // Convert BGRA (with stride) into tightly packed RGBA.
    let mut rgba = vec![0u8; width_usize * height_usize * 4];
    for y in 0..height_usize {
        let in_row = &bgra[y * stride_bytes..y * stride_bytes + width_usize * 4];
        let out_row = &mut rgba[y * width_usize * 4..(y + 1) * width_usize * 4];
        for x in 0..width_usize {
            let i = x * 4;
            out_row[i] = in_row[i + 2];
            out_row[i + 1] = in_row[i + 1];
            out_row[i + 2] = in_row[i];
            out_row[i + 3] = in_row[i + 3];
        }
    }

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().location(loc!())?;
        writer.write_image_data(&rgba).location(loc!())?;
    }
    Ok(out)
}

pub fn decode_png_to_bgra(png_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().location(loc!())?;
    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow!("png output buffer size unknown"))
        .location(loc!())?;
    let mut buf = vec![0u8; buf_size];
    let info = reader.next_frame(&mut buf).location(loc!())?;
    let bytes = &buf[..info.buffer_size()];

    // Normalize into BGRA8.
    let mut bgra = vec![0u8; info.width as usize * info.height as usize * 4];
    match info.color_type {
        png::ColorType::Rgb => {
            ensure!(info.bit_depth == png::BitDepth::Eight, "unsupported PNG bit depth");
            for (i, pixel) in bytes.chunks_exact(3).enumerate() {
                let o = i * 4;
                bgra[o] = pixel[2];
                bgra[o + 1] = pixel[1];
                bgra[o + 2] = pixel[0];
                bgra[o + 3] = 255;
            }
        }
        png::ColorType::Rgba => {
            ensure!(info.bit_depth == png::BitDepth::Eight, "unsupported PNG bit depth");
            for (i, pixel) in bytes.chunks_exact(4).enumerate() {
                let o = i * 4;
                bgra[o] = pixel[2];
                bgra[o + 1] = pixel[1];
                bgra[o + 2] = pixel[0];
                bgra[o + 3] = pixel[3];
            }
        }
        other => bail!("unsupported PNG color type: {other:?}"),
    }

    Ok((info.width, info.height, bgra))
}

pub fn encode_jpeg_from_bgra(
    width: u32,
    height: u32,
    stride_bytes: usize,
    bgra: &[u8],
) -> Result<Vec<u8>> {
    #[cfg(feature = "image-jpeg")]
    {
        let height_usize = height as usize;
        let width_usize = width as usize;
        ensure!(width_usize != 0 && height_usize != 0, "invalid image size");
        ensure!(
            stride_bytes >= width_usize * 4,
            "stride too small: stride={stride_bytes} width={width}"
        );
        ensure!(
            bgra.len() >= stride_bytes.saturating_mul(height_usize),
            "bgra buffer too small: len={} expected_at_least={}",
            bgra.len(),
            stride_bytes.saturating_mul(height_usize)
        );

        let mut rgb = vec![0u8; width_usize * height_usize * 3];
        for y in 0..height_usize {
            let in_row = &bgra[y * stride_bytes..y * stride_bytes + width_usize * 4];
            let out_row = &mut rgb[y * width_usize * 3..(y + 1) * width_usize * 3];
            for x in 0..width_usize {
                let i = x * 4;
                let o = x * 3;
                out_row[o] = in_row[i + 2];
                out_row[o + 1] = in_row[i + 1];
                out_row[o + 2] = in_row[i];
            }
        }

        let mut out = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut out, 85);
        enc.encode(&rgb, width as u16, height as u16, jpeg_encoder::ColorType::Rgb)
            .location(loc!())?;
        Ok(out)
    }

    #[cfg(not(feature = "image-jpeg"))]
    {
        let _ = (width, height, stride_bytes, bgra);
        bail!("JPEG transport codec requires the image-jpeg feature")
    }
}

pub fn decode_jpeg_to_bgra(jpeg_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    #[cfg(feature = "image-jpeg")]
    {
        let mut decoder = jpeg_decoder::Decoder::new(jpeg_bytes);
        let pixels = decoder.decode().location(loc!())?;
        let info = decoder.info().ok_or_else(|| anyhow!("missing JPEG info"))?;
        let width = info.width as u32;
        let height = info.height as u32;

        // jpeg-decoder returns RGB8.
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        for (i, pixel) in pixels.chunks_exact(3).enumerate() {
            let o = i * 4;
            bgra[o] = pixel[2];
            bgra[o + 1] = pixel[1];
            bgra[o + 2] = pixel[0];
            bgra[o + 3] = 255;
        }
        Ok((width, height, bgra))
    }

    #[cfg(not(feature = "image-jpeg"))]
    {
        let _ = jpeg_bytes;
        bail!("JPEG transport codec requires the image-jpeg feature")
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum TransportScope {
    Global,
    Surface(WlSurfaceId),
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
        let mut supported_codecs = vec![
            TransportCodec::default(),
            TransportCodec::ShardedLz4,
            TransportCodec::ShardedRaw,
            TransportCodec::Png,
        ];
        #[cfg(feature = "image-jpeg")]
        supported_codecs.push(TransportCodec::Jpeg);
        #[cfg(feature = "video-h264")]
        supported_codecs.push(TransportCodec::H264);

        Self {
            supported_codecs,
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
    ConfigScoped { scope: TransportScope, config: TransportConfig },
    Pong(Pong),
}

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub enum TransportEvent {
    ClientHello(ClientHello),
    Ping(Ping),
    Stats(TransportStats),
}
