const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d");
const params = new URLSearchParams(location.search);
const surfaceId = params.get("surface");

if (!surfaceId) {
  document.body.textContent = "missing surface id";
}

function connect() {
  const wsUrl = `ws://${location.host}/ws`;
  const ws = new WebSocket(wsUrl);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {};
  ws.onclose = () => {
    setTimeout(connect, 1000);
  };

  ws.onmessage = async (event) => {
    if (typeof event.data === "string") {
      return;
    }

    const buf = new Uint8Array(event.data);
    if (buf.length < 16) return;

    const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const id = view.getBigUint64(0, true).toString();
    if (id !== surfaceId) return;

    const width = view.getUint32(8, true);
    const height = view.getUint32(12, true);
    const payload = buf.slice(16);

    const blob = new Blob([payload], { type: "image/png" });
    const img = await createImageBitmap(blob);
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
    }
    ctx.drawImage(img, 0, 0);
  };
}

connect();
