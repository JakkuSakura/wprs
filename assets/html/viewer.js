const statusEl = document.getElementById("status");
const tabsEl = document.getElementById("tabs");
const messageEl = document.getElementById("message");
const canvas = document.getElementById("canvas");

const surfaces = new Map();
let selectedId = null;

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

function updateTexture(width, height, rgba) {
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }

  if (!texture || textureWidth !== width || textureHeight !== height) {
    texture = device.createTexture({
      size: { width, height },
      format: "rgba8unorm",
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
    rgba,
    { bytesPerRow: width * 4 },
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

function displayTitle(info) {
  return info.title || info.app_id || `surface-${info.id}`;
}

function selectSurface(id) {
  selectedId = id;
  renderTabs();
  messageEl.textContent = `Waiting for frames for ${displayTitle(surfaces.get(id))}...`;
  messageEl.hidden = false;
  canvas.hidden = true;
}

function renderTabs() {
  tabsEl.innerHTML = "";
  if (surfaces.size === 0) {
    tabsEl.textContent = "No windows";
    messageEl.textContent = "No windows captured. Make sure the target app is running and screen capture permissions are granted.";
    messageEl.hidden = false;
    canvas.hidden = true;
    return;
  }

  for (const info of surfaces.values()) {
    const tab = document.createElement("div");
    tab.className = "tab" + (info.id === selectedId ? " active" : "");
    tab.textContent = displayTitle(info);
    tab.onclick = () => selectSurface(info.id);
    tabsEl.appendChild(tab);
  }
}

function connect() {
  const wsUrl = `ws://${location.host}/ws`;
  const ws = new WebSocket(wsUrl);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {
    statusEl.textContent = `connected: ${wsUrl}`;
  };
  ws.onclose = () => {
    statusEl.textContent = "disconnected; retrying...";
    setTimeout(connect, 1000);
  };
  ws.onerror = () => {
    statusEl.textContent = "websocket error";
  };

  ws.onmessage = (event) => {
    if (typeof event.data === "string") {
      const msg = JSON.parse(event.data);
      if (msg.type === "surface") {
        const info = {
          id: msg.id,
          title: msg.title || null,
          app_id: msg.app_id || null,
        };
        surfaces.set(msg.id, info);
        if (!selectedId) {
          selectedId = msg.id;
        }
        renderTabs();
      } else if (msg.type === "surface_destroyed") {
        surfaces.delete(msg.id);
        if (selectedId === msg.id) {
          selectedId = surfaces.keys().next().value || null;
        }
        renderTabs();
      }
      return;
    }

    const buf = new Uint8Array(event.data);
    if (buf.length < 17) return;
    if (buf[0] !== 1) return;

    const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const id = view.getBigUint64(1, true).toString();
    if (id !== selectedId) return;

    const width = view.getUint32(9, true);
    const height = view.getUint32(13, true);
    const payload = buf.slice(17);
    if (payload.length < width * height * 4) return;

    messageEl.hidden = true;
    canvas.hidden = false;
    updateTexture(width, height, payload);
  };
}

initWebGpu()
  .then(() => connect())
  .catch((err) => {
    messageEl.textContent = err.message || String(err);
    messageEl.hidden = false;
    canvas.hidden = true;
  });
