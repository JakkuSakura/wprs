const statusEl = document.getElementById("status");
const listEl = document.getElementById("surface-list");
const emptyEl = document.getElementById("empty");

const surfaces = new Map();

function displayTitle(info) {
  return info.title || info.app_id || `surface-${info.id}`;
}

function openSurface(info) {
  const url = `/view.html?surface=${encodeURIComponent(info.id)}`;
  const win = window.open(url, `_blank`);
  if (win) {
    info.opened = true;
    info.popupBlocked = false;
  } else {
    info.popupBlocked = true;
  }
  renderList();
}

function renderList() {
  listEl.innerHTML = "";
  if (surfaces.size === 0) {
    emptyEl.hidden = false;
    emptyEl.textContent = "Waiting for windows...";
    return;
  }
  emptyEl.hidden = true;

  for (const info of surfaces.values()) {
    const row = document.createElement("div");
    row.className = "surface-row";

    const title = document.createElement("div");
    title.className = "surface-title";
    title.textContent = displayTitle(info);

    const meta = document.createElement("div");
    meta.className = "surface-meta";
    meta.textContent = info.app_id || "";

    const btn = document.createElement("button");
    btn.className = "surface-btn";
    btn.textContent = info.opened ? "Opened" : "Open";
    btn.disabled = info.opened && !info.popupBlocked;
    btn.onclick = () => openSurface(info);

    row.appendChild(title);
    row.appendChild(meta);
    row.appendChild(btn);
    listEl.appendChild(row);
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
        let info = surfaces.get(msg.id);
        if (!info) {
          info = {
            id: msg.id,
            title: msg.title || null,
            app_id: msg.app_id || null,
            opened: false,
            popupBlocked: false,
          };
          surfaces.set(msg.id, info);
          openSurface(info);
        } else {
          info.title = msg.title || info.title;
          info.app_id = msg.app_id || info.app_id;
        }
        renderList();
      } else if (msg.type === "surface_destroyed") {
        surfaces.delete(msg.id);
        renderList();
      }
      return;
    }
  };
}

connect();
