const canvas = document.getElementById("canvas");
const params = new URLSearchParams(location.search);
const surfaceId = params.get("surface");

if (!surfaceId) {
  document.body.textContent = "missing surface id";
}

const gl = canvas.getContext("webgl2") || canvas.getContext("webgl");
if (!gl) {
  document.body.textContent = "WebGL unavailable";
}

function createShader(type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const info = gl.getShaderInfoLog(shader) || "";
    gl.deleteShader(shader);
    throw new Error(info);
  }
  return shader;
}

function createProgram(vertexSource, fragmentSource) {
  const program = gl.createProgram();
  const vs = createShader(gl.VERTEX_SHADER, vertexSource);
  const fs = createShader(gl.FRAGMENT_SHADER, fragmentSource);
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const info = gl.getProgramInfoLog(program) || "";
    gl.deleteProgram(program);
    throw new Error(info);
  }
  return program;
}

const vertexSource = `
  attribute vec2 a_position;
  attribute vec2 a_texCoord;
  varying vec2 v_texCoord;
  void main() {
    v_texCoord = a_texCoord;
    gl_Position = vec4(a_position, 0.0, 1.0);
  }
`;

const fragmentSource = `
  precision mediump float;
  varying vec2 v_texCoord;
  uniform sampler2D u_texture;
  void main() {
    gl_FragColor = texture2D(u_texture, v_texCoord);
  }
`;

const program = createProgram(vertexSource, fragmentSource);
gl.useProgram(program);

const positionLoc = gl.getAttribLocation(program, "a_position");
const texCoordLoc = gl.getAttribLocation(program, "a_texCoord");

const vertexBuffer = gl.createBuffer();
gl.bindBuffer(gl.ARRAY_BUFFER, vertexBuffer);
const vertices = new Float32Array([
  -1, -1, 0, 1,
   1, -1, 1, 1,
  -1,  1, 0, 0,
   1,  1, 1, 0,
]);
gl.bufferData(gl.ARRAY_BUFFER, vertices, gl.STATIC_DRAW);

const stride = 4 * Float32Array.BYTES_PER_ELEMENT;
const texOffset = 2 * Float32Array.BYTES_PER_ELEMENT;

gl.enableVertexAttribArray(positionLoc);
gl.vertexAttribPointer(positionLoc, 2, gl.FLOAT, false, stride, 0);

gl.enableVertexAttribArray(texCoordLoc);
gl.vertexAttribPointer(texCoordLoc, 2, gl.FLOAT, false, stride, texOffset);

const texture = gl.createTexture();
gl.bindTexture(gl.TEXTURE_2D, texture);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

gl.clearColor(0.1, 0.1, 0.1, 1.0);

function updateTexture(width, height, rgba) {
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  gl.viewport(0, 0, width, height);
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, rgba);
  gl.clear(gl.COLOR_BUFFER_BIT);
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
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

connect();
