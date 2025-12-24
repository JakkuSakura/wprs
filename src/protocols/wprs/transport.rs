use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

use crate::prelude::*;

use super::capabilities::CpuFeatures;
use super::capabilities::GpuFeatures;
use super::wayland::WlSurfaceId;

/// User preferences for transport tuning.
#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct TransportPreferences {
    pub usage_goal: Option<UsageGoal>,
    /// Optional drop tolerance hint for quality-of-service decisions.
    pub drop_tolerance: Option<DropTolerance>,
    /// Optional retransmit policy hint for quality-of-service decisions.
    pub retransmit_policy: Option<RetransmitPolicy>,
    /// How the server should select and update transport settings.
    pub selection_mode: SelectionMode,
    /// Explicit codec selection used when selection_mode is Manual.
    pub manual_codec: Option<TransportCodec>,
    /// Optional max bitrate cap, in kilobits/sec.
    pub max_bitrate_kbps: Option<u32>,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum DropTolerance {
    Allow,
    Avoid,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum RetransmitPolicy {
    Allow,
    Avoid,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub enum SelectionMode {
    /// Manual selection: ignore heuristics and use the configured codec.
    Manual,
    /// Static selection: use heuristics once without runtime adjustments.
    Static,
    /// Dynamic selection: allow runtime adjustments.
    Dynamic,
}


impl Default for TransportPreferences {
    fn default() -> Self {
        Self {
            usage_goal: None,
            drop_tolerance: None,
            retransmit_policy: None,
            selection_mode: SelectionMode::Dynamic,
            manual_codec: None,
            max_bitrate_kbps: None,
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
    ensure!(
        width_usize != 0 && height_usize != 0,
        Error::InvalidArgument("invalid image size".to_string()),
    );
    ensure!(
        stride_bytes >= width_usize * 4,
        Error::InvalidArgument(format!(
            "stride too small: stride={stride_bytes} width={width}"
        )),
    );
    ensure!(
        bgra.len() >= stride_bytes.saturating_mul(height_usize),
        Error::InvalidArgument(format!(
            "bgra buffer too small: len={} expected_at_least={}",
            bgra.len(),
            stride_bytes.saturating_mul(height_usize)
        )),
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
        .ok_or_else(|| Error::Internal("png output buffer size unknown".to_string()))
        .location(loc!())?;
    let mut buf = vec![0u8; buf_size];
    let info = reader.next_frame(&mut buf).location(loc!())?;
    let bytes = &buf[..info.buffer_size()];

    // Normalize into BGRA8.
    let mut bgra = vec![0u8; info.width as usize * info.height as usize * 4];
    match info.color_type {
        png::ColorType::Rgb => {
            ensure!(
                info.bit_depth == png::BitDepth::Eight,
                Error::Unsupported("unsupported PNG bit depth".to_string()),
            );
            for (i, pixel) in bytes.chunks_exact(3).enumerate() {
                let o = i * 4;
                bgra[o] = pixel[2];
                bgra[o + 1] = pixel[1];
                bgra[o + 2] = pixel[0];
                bgra[o + 3] = 255;
            }
        }
        png::ColorType::Rgba => {
            ensure!(
                info.bit_depth == png::BitDepth::Eight,
                Error::Unsupported("unsupported PNG bit depth".to_string()),
            );
            for (i, pixel) in bytes.chunks_exact(4).enumerate() {
                let o = i * 4;
                bgra[o] = pixel[2];
                bgra[o + 1] = pixel[1];
                bgra[o + 2] = pixel[0];
                bgra[o + 3] = pixel[3];
            }
        }
        other => bail!(Error::Unsupported(format!(
            "unsupported PNG color type: {other:?}"
        ))),
    }

    Ok((info.width, info.height, bgra))
}

pub fn encode_jpeg_from_bgra(
    width: u32,
    height: u32,
    stride_bytes: usize,
    bgra: &[u8],
) -> Result<Vec<u8>> {
    encode_jpeg_from_bgra_with_quality(width, height, stride_bytes, bgra, 85)
}

pub fn encode_jpeg_from_bgra_with_quality(
    width: u32,
    height: u32,
    stride_bytes: usize,
    bgra: &[u8],
    quality: u8,
) -> Result<Vec<u8>> {
    let height_usize = height as usize;
    let width_usize = width as usize;
    ensure!(
        width_usize != 0 && height_usize != 0,
        Error::InvalidArgument("invalid image size".to_string()),
    );
    ensure!(
        stride_bytes >= width_usize * 4,
        Error::InvalidArgument(format!(
            "stride too small: stride={stride_bytes} width={width}"
        )),
    );
    ensure!(
        bgra.len() >= stride_bytes.saturating_mul(height_usize),
        Error::InvalidArgument(format!(
            "bgra buffer too small: len={} expected_at_least={}",
            bgra.len(),
            stride_bytes.saturating_mul(height_usize)
        )),
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
    let quality = quality.clamp(1, 100);
    let enc = jpeg_encoder::Encoder::new(&mut out, quality);
    enc.encode(&rgb, width as u16, height as u16, jpeg_encoder::ColorType::Rgb)
        .location(loc!())?;
    Ok(out)
}

pub fn decode_jpeg_to_bgra(jpeg_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let mut decoder = jpeg_decoder::Decoder::new(jpeg_bytes);
    let pixels = decoder.decode().location(loc!())?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::Missing("JPEG info".to_string()))?;
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
    /// Optional negotiated JPEG quality (1..=100).
    pub jpeg_quality: Option<u8>,
    /// Optional negotiated H.264 target bitrate in kbps.
    pub h264_bitrate_kbps: Option<u32>,
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
            jpeg_quality: None,
            h264_bitrate_kbps: None,
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
