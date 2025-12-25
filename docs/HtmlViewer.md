# HTML Viewer

## Resilience Plan (Plan A)

Goal: keep the HTML viewer usable under bad network conditions without changing the
core WPRS protocol or requiring a WASM client.

Approach: add a lightweight resilience layer on top of the current WebSocket stream.

- Backpressure: keep at most 1-2 frames per surface pending in the server; drop
  older frames when the client is behind.
- Sequence numbers: attach a monotonically increasing sequence per surface so the
  client can ignore stale frames.
- Client acknowledgements: send a small ack from the browser after presenting a
  frame (surface id + last rendered seq + render time). The server uses this to
  slow down or skip frames.
- Keyframe cadence: periodically send a full frame (every N frames or seconds) to
  ensure recovery after drops.
- Drop policy: prefer dropping intermediate frames instead of queueing, keeping
  latency bounded.

Expected impact:
- Stable behavior on high latency or loss.
- Lower memory usage due to bounded queues.
- Clearer observability (per-surface frame age and drop counters).
