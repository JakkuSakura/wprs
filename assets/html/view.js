const params = new URLSearchParams(window.location.search);
const surfaceId = params.get("surface");
const statusEl = document.createElement("div");
statusEl.style.position = "fixed";
statusEl.style.top = "8px";
statusEl.style.left = "8px";
statusEl.style.padding = "4px 8px";
statusEl.style.background = "rgba(0,0,0,0.6)";
statusEl.style.color = "#fff";
statusEl.style.fontFamily = "sans-serif";
statusEl.style.fontSize = "12px";
statusEl.style.zIndex = "10";
document.body.appendChild(statusEl);

const statusBar = document.createElement("div");
statusBar.style.position = "fixed";
statusBar.style.top = "36px";
statusBar.style.left = "8px";
statusBar.style.right = "8px";
statusBar.style.padding = "6px 10px";
statusBar.style.background = "rgba(0,0,0,0.65)";
statusBar.style.color = "#fff";
statusBar.style.fontFamily = "sans-serif";
statusBar.style.fontSize = "12px";
statusBar.style.zIndex = "10";
statusBar.style.display = "none";
document.body.appendChild(statusBar);

const canvas = document.getElementById("canvas");

let gpuContext;
let device;
let texture;
let textureWidth = 0;
let textureHeight = 0;
let sampler;
let pipeline;
let bindGroup;
let vertexBuffer;
let indexBuffer;
let canvasFormat;
let lastFrameAt = 0;
let frameCount = 0;

function showStatus(message, kind) {
  statusBar.textContent = message;
  if (kind === "error") {
    statusBar.style.background = "rgba(120,0,0,0.85)";
  } else if (kind === "warn") {
    statusBar.style.background = "rgba(120,80,0,0.85)";
  } else {
    statusBar.style.background = "rgba(0,0,0,0.65)";
  }
  statusBar.style.display = "block";
}

function clearStatus() {
  statusBar.style.display = "none";
}

async function initWebGpu() {
  if (!navigator.gpu) {
    throw new Error("WebGPU unavailable");
  }

  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) {
    throw new Error("WebGPU adapter unavailable");
  }

  device = await adapter.requestDevice();
  gpuContext = canvas.getContext("webgpu");
  canvasFormat = navigator.gpu.getPreferredCanvasFormat();
  gpuContext.configure({
    device,
    format: canvasFormat,
    alphaMode: "opaque",
  });

  const shaderModule = device.createShaderModule({
    code: `
      struct VertexOut {
        @builtin(position) position: vec4<f32>,
        @location(0) uv: vec2<f32>,
      };

      @vertex
      fn vs_main(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> VertexOut {
        var out: VertexOut;
        out.position = vec4<f32>(position, 0.0, 1.0);
        out.uv = uv;
        return out;
      }

      @group(0) @binding(0) var tex: texture_2d<f32>;
      @group(0) @binding(1) var samp: sampler;

      @fragment
      fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
        return textureSample(tex, samp, uv);
      }
    `,
  });

  const bindGroupLayout = device.createBindGroupLayout({
    entries: [
      { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: {} },
      { binding: 1, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
    ],
  });

  pipeline = device.createRenderPipeline({
    layout: device.createPipelineLayout({ bindGroupLayouts: [bindGroupLayout] }),
    vertex: {
      module: shaderModule,
      entryPoint: "vs_main",
      buffers: [
        {
          arrayStride: 16,
          attributes: [
            { shaderLocation: 0, offset: 0, format: "float32x2" },
            { shaderLocation: 1, offset: 8, format: "float32x2" },
          ],
        },
      ],
    },
    fragment: {
      module: shaderModule,
      entryPoint: "fs_main",
      targets: [{ format: canvasFormat }],
    },
    primitive: { topology: "triangle-list" },
  });

  const vertices = new Float32Array([
    -1, -1, 0, 1,
     1, -1, 1, 1,
     1,  1, 1, 0,
    -1,  1, 0, 0,
  ]);
  vertexBuffer = device.createBuffer({
    size: vertices.byteLength,
    usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
  });
  device.queue.writeBuffer(vertexBuffer, 0, vertices);

  const indices = new Uint16Array([0, 1, 2, 2, 3, 0]);
  indexBuffer = device.createBuffer({
    size: indices.byteLength,
    usage: GPUBufferUsage.INDEX | GPUBufferUsage.COPY_DST,
  });
  device.queue.writeBuffer(indexBuffer, 0, indices);

  sampler = device.createSampler({
    magFilter: "nearest",
    minFilter: "nearest",
  });
}

function updateTexture(width, height, bgra, stride, scale) {
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  const cssScale = scale && scale > 0 ? scale : 1;
  const cssWidth = Math.max(1, Math.round(width / cssScale));
  const cssHeight = Math.max(1, Math.round(height / cssScale));
  if (canvas.style.width !== `${cssWidth}px` || canvas.style.height !== `${cssHeight}px`) {
    canvas.style.width = `${cssWidth}px`;
    canvas.style.height = `${cssHeight}px`;
  }

  if (!texture || textureWidth !== width || textureHeight !== height) {
    texture = device.createTexture({
      size: { width, height },
      format: "bgra8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    textureWidth = width;
    textureHeight = height;

    bindGroup = device.createBindGroup({
      layout: pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: texture.createView() },
        { binding: 1, resource: sampler },
      ],
    });
  }

  device.queue.writeTexture(
    { texture },
    bgra,
    { bytesPerRow: stride },
    { width, height }
  );

  const encoder = device.createCommandEncoder();
  const pass = encoder.beginRenderPass({
    colorAttachments: [
      {
        view: gpuContext.getCurrentTexture().createView(),
        loadOp: "clear",
        storeOp: "store",
        clearValue: { r: 0.1, g: 0.1, b: 0.1, a: 1.0 },
      },
    ],
  });
  pass.setPipeline(pipeline);
  pass.setBindGroup(0, bindGroup);
  pass.setVertexBuffer(0, vertexBuffer);
  pass.setIndexBuffer(indexBuffer, "uint16");
  pass.drawIndexed(6);
  pass.end();
  device.queue.submit([encoder.finish()]);
}

function connect() {
  if (!surfaceId) {
    statusEl.textContent = "missing ?surface=...";
    showStatus("Missing surface id. Open this page via the main viewer list.", "error");
    return;
  }

  const wsUrl = `ws://${location.host}/ws`;
  const ws = new WebSocket(wsUrl);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {
    statusEl.textContent = `connected: ${wsUrl} (surface ${surfaceId})`;
    clearStatus();
    ws.send(JSON.stringify({ type: "subscribe", id: surfaceId }));
  };
  ws.onclose = () => {
    statusEl.textContent = "disconnected; retrying...";
    showStatus("WebSocket disconnected. Retrying...", "warn");
    setTimeout(connect, 1000);
  };
  ws.onerror = () => {
    statusEl.textContent = "websocket error";
    showStatus("WebSocket error. Check the server and network.", "error");
  };

  ws.onmessage = (event) => {
    if (typeof event.data === "string") {
      const msg = JSON.parse(event.data);
      if (msg.type === "surface" && msg.id === surfaceId) {
        if (msg.title) {
          document.title = msg.title;
        }
      }
      return;
    }

    const buf = new Uint8Array(event.data);
    if (buf.length < 25) {
      showStatus("Invalid frame: header too short", "error");
      return;
    }
    if (buf[0] !== 1) {
      showStatus("Invalid frame: bad type byte", "error");
      return;
    }

    const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const id = view.getBigUint64(1, true).toString();
    if (id !== surfaceId) return;

    const width = view.getUint32(9, true);
    const height = view.getUint32(13, true);
    const stride = view.getUint32(17, true);
    const scale = view.getUint32(21, true);
    const payload = buf.slice(25);
    const expected = stride * height;
    if (payload.length < expected) {
      showStatus(`Invalid frame: payload too short (${payload.length} < ${expected})`, "error");
      return;
    }

    updateTexture(width, height, payload, stride, scale);
    lastFrameAt = Date.now();
    frameCount += 1;
    if (frameCount === 1) {
      clearStatus();
    }
  };
}

setInterval(() => {
  if (lastFrameAt === 0) {
    showStatus("No frames received yet.", "warn");
    return;
  }
  const ageMs = Date.now() - lastFrameAt;
  if (ageMs > 5000) {
    showStatus(`No frames for ${(ageMs / 1000).toFixed(1)}s`, "warn");
  } else {
    clearStatus();
  }
}, 2000);

initWebGpu()
  .then(() => connect())
  .catch((err) => {
    statusEl.textContent = err.message || String(err);
  });
