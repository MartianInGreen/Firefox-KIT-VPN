/*
 * background.js — KIT VPN per-site proxy routing.
 *
 * Decides, per request:
 *   configured KIT domains -> localhost SOCKS5 (isolated KIT OpenVPN tunnel)
 *   everything else        -> DIRECT (normal system connection)
 *
 * Speaks Native Messaging to the local companion (kit_vpn_companion) which
 * owns the tunnel lifecycle and reports status.
 */
"use strict";

const NATIVE_NAME = "kit_vpn_companion";
const DEFAULT_DOMAINS = ["*.kit.edu"];
const DEFAULT_SOCKS_PORT = 1080;
const DIRECT = { type: "direct" };
const POLL_INTERVAL_MS = 3000;

const state = {
  enabled: true, // per-site KIT-only routing; non-KIT traffic is never affected
  domains: DEFAULT_DOMAINS.slice(),
  socksPort: DEFAULT_SOCKS_PORT,
  status: { state: "unknown", detail: "", socks_port: DEFAULT_SOCKS_PORT },
};

/* ------------------------------------------------------------------ */
/* persistence                                                         */
/* ------------------------------------------------------------------ */

async function loadState() {
  try {
    const s = await browser.storage.local.get(["enabled", "domains"]);
    if (typeof s.enabled === "boolean") state.enabled = s.enabled;
    if (Array.isArray(s.domains) && s.domains.length) state.domains = s.domains;
  } catch (e) {
    console.error("loadState:", e);
  }
}

async function saveState() {
  return browser.storage.local.set({ enabled: state.enabled, domains: state.domains });
}

browser.storage.onChanged.addListener((changes, area) => {
  if (area !== "local") return;
  if (changes.enabled) state.enabled = changes.enabled.newValue;
  if (changes.domains) state.domains = changes.domains.newValue || [];
});

/* ------------------------------------------------------------------ */
/* per-request proxy decision                                          */
/* ------------------------------------------------------------------ */

browser.proxy.onRequest.addListener(
  (req) => {
    if (!state.enabled) return DIRECT;
    const host = kitHostname(req.url);
    if (host && kitMatches(host, state.domains)) {
      // SOCKS5 on the local companion; proxyDNS = true resolves hostnames
      // inside the tunnel (no DNS leak for internal KIT names).
      return {
        type: "socks",
        host: "127.0.0.1",
        port: state.socksPort,
        proxyDNS: true,
      };
    }
    return DIRECT;
  },
  { urls: ["<all_urls>"] }
);

browser.proxy.onError.addListener((err) => {
  console.error("KIT VPN proxy error:", err && err.message ? err.message : err);
});

/* ------------------------------------------------------------------ */
/* native messaging with the companion                                 */
/* ------------------------------------------------------------------ */

let port = null;
let reconnectTimer = null;

function connectNative() {
  try {
    port = browser.runtime.connectNative(NATIVE_NAME);
  } catch (e) {
    applyStatus({ state: "error", detail: "cannot connect to companion: " + e.message });
    return;
  }
  port.onMessage.addListener(onNativeMessage);
  port.onDisconnect.addListener(() => {
    port = null;
    const why = browser.runtime.lastError ? browser.runtime.lastError.message : "disconnected";
    applyStatus({ state: "unavailable", detail: "companion unavailable: " + why });
    if (reconnectTimer) clearTimeout(reconnectTimer);
    reconnectTimer = setTimeout(connectNative, 3000);
  });
}

function sendNative(msg) {
  if (!port) {
    console.warn("KIT VPN: no native port (companion not reachable)");
    return;
  }
  try {
    port.postMessage(msg);
  } catch (e) {
    console.error("sendNative:", e);
  }
}

function onNativeMessage(msg) {
  if (!msg || typeof msg !== "object") return;
  if (msg.type === "status" || msg.type === "enable" || msg.type === "disable") {
    applyStatus(msg);
  }
}

function applyStatus(msg) {
  const st = {
    state: String(msg.state || "unknown"),
    detail: String(msg.detail || ""),
    socks_port: Number(msg.socks_port) || DEFAULT_SOCKS_PORT,
  };
  state.status = st;
  if (st.socks_port) state.socksPort = st.socks_port;
}

function pollStatus() {
  sendNative({ type: "status" });
}

/* ------------------------------------------------------------------ */
/* commands from the popup                                             */
/* ------------------------------------------------------------------ */

async function setEnabled(v) {
  state.enabled = !!v;
  await saveState();
  // idempotent on the companion side
  sendNative({ type: v ? "enable" : "disable" });
  pollStatus();
}

function addDomain(pattern) {
  const norm = kitNormalizeDomain(pattern);
  if (!norm) return { ok: false, error: "Invalid domain pattern." };
  const raw = String(pattern).trim();
  if (state.domains.indexOf(raw) === -1) state.domains.push(raw);
  saveState();
  return { ok: true, domains: state.domains.slice() };
}

function removeDomain(pattern) {
  state.domains = state.domains.filter((d) => d !== pattern);
  saveState();
  return { ok: true, domains: state.domains.slice() };
}

browser.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  try {
    switch (msg && msg.type) {
      case "getState":
        sendResponse({
          enabled: state.enabled,
          domains: state.domains.slice(),
          status: state.status,
          socksPort: state.socksPort,
          runtimeId: browser.runtime.id,
        });
        break;
      case "setEnabled":
        setEnabled(msg.value);
        sendResponse({ ok: true });
        break;
      case "addDomain":
        sendResponse(addDomain(msg.domain));
        break;
      case "removeDomain":
        sendResponse(removeDomain(msg.domain));
        break;
      default:
        sendResponse({ ok: false, error: "unknown message" });
    }
  } catch (e) {
    sendResponse({ ok: false, error: String(e) });
  }
});

/* ------------------------------------------------------------------ */
/* startup                                                             */
/* ------------------------------------------------------------------ */

(async function init() {
  console.log("KIT VPN extension id:", browser.runtime.id);
  await loadState();
  connectNative();
  pollStatus();
  setInterval(pollStatus, POLL_INTERVAL_MS);
  if (state.enabled) {
    sendNative({ type: "enable" });
  }
})();
