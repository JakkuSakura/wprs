use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn generate_rgba_frame(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut s = seed;
    for px in rgba.chunks_exact_mut(4) {
        // xorshift64*
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let v = s.wrapping_mul(0x2545F4914F6CDD1D);
        px[0] = (v & 0xFF) as u8;
        px[1] = ((v >> 8) & 0xFF) as u8;
        px[2] = ((v >> 16) & 0xFF) as u8;
        px[3] = 0xFF;
    }
    rgba
}

#[cfg(feature = "video-h264")]
fn rgba_to_bgra(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    rgba
}

#[cfg(not(feature = "video-h264"))]
#[allow(dead_code)]
fn rgba_to_bgra(_rgba: Vec<u8>) -> Vec<u8> {
    unreachable!("video-h264 disabled")
}

fn bench_png_static(c: &mut Criterion) {
    let (width, height) = (1280u32, 720u32);
    let rgba = generate_rgba_frame(width, height, 0x1234_5678_90AB_CDEF);

    c.bench_function("image/png/static/1280x720", |b| {
        b.iter(|| {
            let encoded = wprs::protocols::image::png::encode_png_rgba(&rgba, width, height)
                .expect("png encode must succeed");
            black_box(encoded.len());
        })
    });
}

fn bench_png_dynamic(c: &mut Criterion) {
    let (width, height) = (1280u32, 720u32);
    let frames: Vec<Vec<u8>> = (0..30)
        .map(|i| generate_rgba_frame(width, height, 0xBADC_0FFE_EE00_0000u64 + i as u64))
        .collect();

    c.bench_function("image/png/dynamic/1280x720", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let rgba = &frames[idx];
            idx = (idx + 1) % frames.len();
            let encoded = wprs::protocols::image::png::encode_png_rgba(rgba, width, height)
                .expect("png encode must succeed");
            black_box(encoded.len());
        })
    });
}

#[cfg(feature = "video-h264")]
fn bench_h264_static(c: &mut Criterion) {
    let (width, height) = (1280u32, 720u32);
    let rgba = generate_rgba_frame(width, height, 0x1234_5678_90AB_CDEF);
    let bgra = rgba_to_bgra(rgba);
    let row_bytes = width as usize * 4;

    let mut encoder = wprs::protocols::video::h264::H264Encoder::new(width, height, 60)
        .expect("h264 encoder must be available");

    c.bench_function("video/h264/static/1280x720", |b| {
        b.iter(|| {
            let packet = encoder
                .encode(&bgra, row_bytes)
                .expect("h264 encode must succeed");
            black_box(packet.len());
        })
    });
}

#[cfg(not(feature = "video-h264"))]
fn bench_h264_static(_: &mut Criterion) {}

#[cfg(feature = "video-h264")]
fn bench_h264_dynamic(c: &mut Criterion) {
    let (width, height) = (1280u32, 720u32);
    let row_bytes = width as usize * 4;

    let frames: Vec<Vec<u8>> = (0..60)
        .map(|i| rgba_to_bgra(generate_rgba_frame(width, height, 0xD00D_F00D_0000_0000u64 + i as u64)))
        .collect();

    let mut encoder = wprs::protocols::video::h264::H264Encoder::new(width, height, 60)
        .expect("h264 encoder must be available");

    c.bench_function("video/h264/dynamic/1280x720", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let bgra = &frames[idx];
            idx = (idx + 1) % frames.len();
            let packet = encoder
                .encode(bgra, row_bytes)
                .expect("h264 encode must succeed");
            black_box(packet.len());
        })
    });
}

#[cfg(not(feature = "video-h264"))]
fn bench_h264_dynamic(_: &mut Criterion) {}

criterion_group!(
    benches,
    bench_png_static,
    bench_png_dynamic,
    bench_h264_static,
    bench_h264_dynamic
);
criterion_main!(benches);
