const statusEl = document.getElementById("status");
const surfacesEl = document.getElementById("surfaces");
const surfaces = new Map();

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
        ensureSurface(msg.id, msg.title || "surface");
      } else if (msg.type === "surface_destroyed") {
        surfaces.delete(msg.id);
        renderSurfaceList();
      }
    }
  };
}

function ensureSurface(id, title) {
  const existing = surfaces.get(id);
  if (!existing) {
    surfaces.set(id, { id, title });
    renderSurfaceList();
    // NOTE: browsers may block popups unless triggered by user interaction.
    window.open(`/view.html?surface=${encodeURIComponent(id)}`, "_blank");
    return;
  }
  if (existing.title !== title) {
    existing.title = title;
    renderSurfaceList();
  }
}

function renderSurfaceList() {
  surfacesEl.innerHTML = "";
  for (const s of surfaces.values()) {
    const row = document.createElement("div");
    row.className = "surface";
    row.textContent = `${s.title} (${s.id})`;

    const button = document.createElement("button");
    button.textContent = "open";
    button.onclick = () => {
      window.open(`/view.html?surface=${encodeURIComponent(s.id)}`, "_blank");
    };

    row.appendChild(button);
    surfacesEl.appendChild(row);
  }
}

connect();
