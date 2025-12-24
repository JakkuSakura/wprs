use std::sync::OnceLock;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::error::EAGAIN;

use crate::prelude::*;

static FFMPEG_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn ensure_ffmpeg() -> Result<()> {
    let init = FFMPEG_INIT.get_or_init(|| ffmpeg::init().map_err(|err| err.to_string()));
    match init {
        Ok(()) => Ok(()),
        Err(err) => bail!(Error::Internal(format!("ffmpeg init failed: {err}"))),
    }
}

pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bgra: Vec<u8>,
}

pub struct H264Encoder {
    encoder: ffmpeg::codec::encoder::video::Encoder,
    scaler: ffmpeg::software::scaling::Context,
    frame: ffmpeg::frame::Video,
    next_pts: i64,
    width: u32,
    height: u32,
    avcc_nal_length_size: Option<usize>,
    annexb_config: Vec<u8>,
    sent_config: bool,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self> {
        ensure_ffmpeg().location(loc!())?;
        #[cfg(target_os = "macos")]
        let codec = ffmpeg::encoder::find_by_name("h264_videotoolbox")
            .or_else(|| ffmpeg::encoder::find(ffmpeg::codec::Id::H264))
            .ok_or_else(|| {
                Error::Unsupported(
                    "H264 encoder not available (try enabling h264_videotoolbox)".to_string(),
                )
            })
            .location(loc!())?;

        #[cfg(not(target_os = "macos"))]
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| Error::Unsupported("H264 encoder not available".to_string()))
            .location(loc!())?;
        let context = ffmpeg::codec::context::Context::new();
        let mut encoder = context.encoder().video().location(loc!())?;
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg::format::Pixel::YUV420P);
        encoder.set_time_base(ffmpeg::Rational::new(1, fps as i32));
        encoder.set_frame_rate(Some(ffmpeg::Rational::new(fps as i32, 1)));
        encoder.set_gop(fps);
        encoder.set_max_b_frames(0);
        let base_rate = width as usize * height as usize * fps as usize;
        encoder.set_bit_rate(base_rate.max(500_000));
        let mut options = ffmpeg::Dictionary::new();
        options.set("preset", "veryfast");
        options.set("tune", "zerolatency");
        options.set("profile", "baseline");
        let encoder = encoder
            .open_as_with(codec, options)
            .map_err(|err| {
                Error::Unsupported(format!(
                    "H264 encoder init failed ({err}); try disabling H264 or install libx264"
                ))
            })
            .location(loc!())?;
        let (avcc_nal_length_size, annexb_config) =
            extract_h264_config(&encoder).unwrap_or((None, Vec::new()));
        let scaler = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::BGRA,
            width,
            height,
            ffmpeg::format::Pixel::YUV420P,
            width,
            height,
            ffmpeg::software::scaling::flag::Flags::FAST_BILINEAR,
        )
        .location(loc!())?;
        let frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, width, height);
        Ok(Self {
            encoder,
            scaler,
            frame,
            next_pts: 0,
            width,
            height,
            avcc_nal_length_size,
            annexb_config,
            sent_config: false,
        })
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Vec<u8>> {
        ensure_ffmpeg().location(loc!())?;
        let mut source =
            ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, self.width, self.height);
        let row_bytes = self.width as usize * 4;
        let src_stride = stride.max(row_bytes);
        let dst_stride = source.stride(0);
        let dst = source.data_mut(0);
        for y in 0..self.height as usize {
            let src = &bgra[y * src_stride..y * src_stride + row_bytes];
            let out_row = &mut dst[y * dst_stride..y * dst_stride + row_bytes];
            out_row.copy_from_slice(src);
        }

        self.scaler.run(&source, &mut self.frame).location(loc!())?;
        self.frame.set_pts(Some(self.next_pts));
        self.next_pts += 1;
        self.encoder.send_frame(&self.frame).location(loc!())?;
        let mut out = Vec::new();
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    if let Some(data) = packet.data() {
                        if !self.sent_config && !self.annexb_config.is_empty() {
                            out.extend_from_slice(&self.annexb_config);
                            self.sent_config = true;
                        }
                        let annexb = match self.avcc_nal_length_size {
                            Some(nal_length_size) if !looks_like_annexb(data) => {
                                match avcc_packet_to_annexb(data, nal_length_size) {
                                    Ok(converted) => converted,
                                    Err(err) => {
                                        warn!("h264 avcc->annexb conversion failed: {err}");
                                        data.to_vec()
                                    }
                                }
                            }
                            _ => data.to_vec(),
                        };
                        out.extend_from_slice(&annexb);
                    }
                }
                Err(err) if err == ffmpeg::Error::Other { errno: EAGAIN } => break,
                Err(err) => {
                    return Err(Error::Internal(format!("ffmpeg encode failed: {err}")))
                        .location(loc!())
                }
            }
        }
        Ok(out)
    }
}

pub struct H264Decoder {
    decoder: ffmpeg::codec::decoder::Video,
    scaler: Option<ffmpeg::software::scaling::Context>,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        ensure_ffmpeg().location(loc!())?;
        let codec = ffmpeg::decoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| Error::Unsupported("H264 decoder not available".to_string()))
            .location(loc!())?;
        let context = ffmpeg::codec::context::Context::new();
        let decoder = context.decoder().open_as(codec).location(loc!())?.video().location(loc!())?;
        Ok(Self {
            decoder,
            scaler: None,
        })
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>> {
        ensure_ffmpeg().location(loc!())?;
        let packet = ffmpeg::Packet::copy(data);
        self.decoder.send_packet(&packet).location(loc!())?;
        let mut decoded = ffmpeg::frame::Video::empty();
        match self.decoder.receive_frame(&mut decoded) {
            Ok(()) => {
                let width = decoded.width();
                let height = decoded.height();
                let format = decoded.format();
                let scaler = match &mut self.scaler {
                    Some(existing) if existing.input().format == format
                        && existing.input().width == width
                        && existing.input().height == height => existing,
                    _ => {
                        self.scaler = Some(
                            ffmpeg::software::scaling::Context::get(
                                format,
                                width,
                                height,
                                ffmpeg::format::Pixel::BGRA,
                                width,
                                height,
                                ffmpeg::software::scaling::flag::Flags::FAST_BILINEAR,
                            )
                            .location(loc!())?,
                        );
                        self.scaler.as_mut().unwrap()
                    },
                };
                let mut bgra = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, width, height);
                scaler.run(&decoded, &mut bgra).location(loc!())?;
                let row_bytes = width as usize * 4;
                let src_stride = bgra.stride(0);
                let mut out = vec![0u8; row_bytes * height as usize];
                for y in 0..height as usize {
                    let src = &bgra.data(0)[y * src_stride..y * src_stride + row_bytes];
                    let dst = &mut out[y * row_bytes..y * row_bytes + row_bytes];
                    dst.copy_from_slice(src);
                }
                Ok(Some(DecodedFrame {
                    width,
                    height,
                    stride: row_bytes,
                    bgra: out,
                }))
            },
            Err(err) if err == ffmpeg::Error::Other { errno: EAGAIN } => Ok(None),
            Err(err) => Err(Error::Internal(format!("ffmpeg decode failed: {err}"))).location(loc!()),
        }
    }
}

fn looks_like_annexb(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])
}

fn extract_h264_config(
    encoder: &ffmpeg::codec::encoder::video::Encoder,
) -> Option<(Option<usize>, Vec<u8>)> {
    unsafe {
        let context = encoder.as_ref();
        let ptr = context.as_ptr();
        if ptr.is_null() {
            return None;
        }
        let size = (*ptr).extradata_size;
        if size <= 0 {
            return None;
        }
        let data = std::slice::from_raw_parts((*ptr).extradata as *const u8, size as usize);
        if looks_like_annexb(data) {
            return Some((None, data.to_vec()));
        }
        avcc_extradata_to_annexb(data)
    }
}

fn avcc_extradata_to_annexb(data: &[u8]) -> Option<(Option<usize>, Vec<u8>)> {
    if data.len() < 7 || data[0] != 1 {
        return None;
    }
    let nal_length_size = ((data[4] & 0x03) + 1) as usize;
    if !(1..=4).contains(&nal_length_size) {
        return None;
    }
    let mut offset = 5;
    let num_sps = (data[offset] & 0x1f) as usize;
    offset += 1;
    let mut annexb = Vec::new();
    for _ in 0..num_sps {
        if offset + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + len > data.len() {
            return None;
        }
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&data[offset..offset + len]);
        offset += len;
    }
    if offset >= data.len() {
        return Some((Some(nal_length_size), annexb));
    }
    let num_pps = data[offset] as usize;
    offset += 1;
    for _ in 0..num_pps {
        if offset + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + len > data.len() {
            return None;
        }
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&data[offset..offset + len]);
        offset += len;
    }
    Some((Some(nal_length_size), annexb))
}

fn avcc_packet_to_annexb(data: &[u8], nal_length_size: usize) -> Result<Vec<u8>> {
    ensure!(
        (1..=4).contains(&nal_length_size),
        Error::InvalidArgument(format!("invalid nal length size {nal_length_size}"))
    );
    let mut out = Vec::with_capacity(data.len() + 64);
    let mut offset = 0usize;
    while offset + nal_length_size <= data.len() {
        let len = read_be_nal_length(&data[offset..offset + nal_length_size]);
        offset += nal_length_size;
        ensure!(
            offset + len <= data.len(),
            Error::InvalidArgument("truncated avcc packet".to_string())
        );
        if len == 0 {
            continue;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[offset..offset + len]);
        offset += len;
    }
    ensure!(
        offset == data.len(),
        Error::InvalidArgument("trailing avcc bytes".to_string())
    );
    Ok(out)
}

fn read_be_nal_length(bytes: &[u8]) -> usize {
    let mut len = 0usize;
    for &b in bytes {
        len = (len << 8) | b as usize;
    }
    len
}
