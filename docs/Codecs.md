# Codecs

This document describes the transport codec implementations and how the server
selects them.

## Codec Implementations

### ShardedZstd / ShardedLz4 / ShardedRaw
- Payload: filtered BGRA shards.
- Encoding: `filtering::filter_and_compress` for zstd/LZ4 or raw filtered bytes.
- Use cases:
  - `ShardedRaw`: lowest CPU, highest bandwidth.
  - `ShardedLz4`: low CPU, moderate compression.
  - `ShardedZstd`: better compression at higher CPU cost.

### PNG
- Payload: PNG-encoded RGBA bytes (lossless).
- Encoding: `transport::encode_png_from_bgra`.
- Best for clarity-first, low-motion surfaces and low error tolerance.

### JPEG
- Payload: JPEG-encoded RGB bytes (lossy).
- Encoding: `transport::encode_jpeg_from_bgra`.
- Best for clarity-biased but bandwidth-constrained scenarios where lossiness is acceptable.

### H.264
- Payload: H.264 bitstream (lossy, temporal compression).
- Encoding: `protocols::video::h264::H264Encoder`.
- Best for high-motion or bandwidth-constrained scenarios when decoding is plausible.

## Selection Model

Selection is multi-objective and behavior-specific:

- Inputs:
  - Capabilities: codecs, CPU/GPU decode hints.
  - Preferences: usage goal, dimension weights, QoS knobs, max bitrate cap.
  - Runtime stats: observed tx kbps, per-surface tx share, per-surface FPS estimate.
  - Client refresh: max refresh rate inferred from output events.

- Behaviors chosen independently:
  - Codec
  - Buffer patches
  - FPS cap

Each behavior has its own dimension weights and a soft penalty for large negative
scores. The engine chooses the combination that maximizes the weighted score
while respecting hard constraints (e.g., max bitrate cap, decode feasibility).

### Behavior Dimensions
- Latency
- Bandwidth
- Clarity
- CPU

### Manual / Static / Dynamic
- Manual: use explicit `manual_codec` if supported and within caps.
- Static: apply heuristics once, ignore runtime adjustment.
- Dynamic: apply heuristics using runtime stats and refresh data.

## Constraints and Guards
- Lossy codecs require `allow_lossy` and decode plausibility.
- Max bitrate cap limits codec choices.
- H.264 is de-prioritized for small, low-FPS surfaces unless pressure is high.
- Per-surface selection uses surface FPS and throughput share to avoid starving
  other surfaces.

## Notes
- Global selection provides a baseline; per-surface selection refines it when
  dynamic selection is enabled.
- Refresh-rate driven FPS caps keep sending aligned with the client display.
