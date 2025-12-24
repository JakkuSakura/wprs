const canvas = document.getElementById("canvas");
const params = new URLSearchParams(location.search);
const surfaceId = params.get("surface");

if (!surfaceId) {
  document.body.textContent = "missing surface id";
}

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

  return true;
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

function connect() {
  const wsUrl = `ws://${location.host}/ws`;
  const ws = new WebSocket(wsUrl);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {};
  ws.onclose = () => {
    setTimeout(connect, 1000);
  };

  ws.onmessage = (event) => {
    if (typeof event.data === "string") {
      return;
    }

    const buf = new Uint8Array(event.data);
    if (buf.length < 17) return;
    if (buf[0] !== 1) return;

    const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const id = view.getBigUint64(1, true).toString();
    if (id !== surfaceId) return;

    const width = view.getUint32(9, true);
    const height = view.getUint32(13, true);
    const payload = buf.slice(17);
    if (payload.length < width * height * 4) return;

    updateTexture(width, height, payload);
  };
}

initWebGpu()
  .then(() => connect())
  .catch((err) => {
    document.body.textContent = err.message || String(err);
  });
