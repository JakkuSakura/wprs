use std::sync::OnceLock;

use ffmpeg_next as ffmpeg;

use crate::prelude::*;

static FFMPEG_INIT: OnceLock<Result<(), String>> = OnceLock::new();

fn ensure_ffmpeg() -> Result<()> {
    let init = FFMPEG_INIT.get_or_init(|| ffmpeg::init().map_err(|err| err.to_string()));
    match init {
        Ok(()) => Ok(()),
        Err(err) => bail!("ffmpeg init failed: {err}"),
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
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self> {
        ensure_ffmpeg().location(loc!())?;
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| anyhow!("H264 encoder not available"))
            .location(loc!())?;
        let mut context = ffmpeg::codec::context::Context::new();
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
        let encoder = encoder.open_as(codec).location(loc!())?;
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
        })
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Vec<u8>> {
        ensure_ffmpeg().location(loc!())?;
        let mut source = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, self.width, self.height);
        let row_bytes = self.width as usize * 4;
        let src_stride = stride.max(row_bytes);
        for y in 0..self.height as usize {
            let src = &bgra[y * src_stride..y * src_stride + row_bytes];
            let dst = &mut source.data_mut(0)[y * source.stride(0)..y * source.stride(0) + row_bytes];
            dst.copy_from_slice(src);
        }

        self.scaler.run(&source, &mut self.frame).location(loc!())?;
        self.frame.set_pts(Some(self.next_pts));
        self.next_pts += 1;
        self.encoder.send_frame(&self.frame).location(loc!())?;
        let mut out = Vec::new();
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => out.extend_from_slice(packet.data()),
                Err(err) if err == ffmpeg::Error::Again => break,
                Err(err) => return Err(anyhow!(err)).location(loc!()),
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
            .ok_or_else(|| anyhow!("H264 decoder not available"))
            .location(loc!())?;
        let mut context = ffmpeg::codec::context::Context::new();
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
            Err(err) if err == ffmpeg::Error::Again => Ok(None),
            Err(err) => Err(anyhow!(err)).location(loc!()),
        }
    }
}
