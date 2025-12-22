# Media benches

This directory contains Criterion benchmarks for media-related codecs.

## Run

Image (PNG):

- `cargo bench --bench media_codecs`

Video (H264) requires system FFmpeg libraries and enables the `video-h264` feature:

- macOS (Homebrew): `brew install ffmpeg`
- then: `cargo bench --bench media_codecs --features video-h264`

Notes:
- The video benchmarks are compiled as no-ops unless `--features video-h264` is enabled.
- The H264 feature depends on `ffmpeg-sys-next` (via `ffmpeg-next`) and will fail to build if
  `libavutil`/`libavcodec`/`libswscale` are not discoverable via `pkg-config`.
