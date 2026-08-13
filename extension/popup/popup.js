/* popup.js — tiny UI for the KIT VPN extension. */
"use strict";

const els = {
  toggle: document.getElementById("toggle"),
  status: document.getElementById("status"),
  list: document.getElementById("domain-list"),
  form: document.getElementById("add-form"),
  input: document.getElementById("new-domain"),
};

function renderState(st) {
  els.toggle.checked = !!st.enabled;

  const stName = st.status && st.status.state ? st.status.state : "unknown";
  const detail = st.status && st.status.detail ? " — " + st.status.detail : "";
  els.status.textContent = stName + detail;
  els.status.className = "status " + stName;
  els.status.title = "extension id: " + (st.runtimeId || "?");

  els.list.textContent = "";
  for (const d of st.domains) {
    const li = document.createElement("li");
    const span = document.createElement("span");
    span.textContent = d;
    span.title = d;
    const rm = document.createElement("button");
    rm.textContent = "✕";
    rm.className = "remove";
    rm.title = "Remove " + d;
    rm.addEventListener("click", () => {
      browser.runtime.sendMessage({ type: "removeDomain", domain: d });
    });
    li.appendChild(span);
    li.appendChild(rm);
    els.list.appendChild(li);
  }
}

async function refresh() {
  try {
    const st = await browser.runtime.sendMessage({ type: "getState" });
    if (st && Array.isArray(st.domains)) renderState(st);
  } catch (e) {
    /* background script not ready yet */
  }
}

els.toggle.addEventListener("change", () => {
  browser.runtime.sendMessage({ type: "setEnabled", value: els.toggle.checked });
});

els.form.addEventListener("submit", (ev) => {
  ev.preventDefault();
  const val = els.input.value.trim();
  if (!val) return;
  browser.runtime.sendMessage({ type: "addDomain", domain: val }).then((r) => {
    if (r && r.ok) {
      els.input.value = "";
      refresh();
    } else {
      window.alert(r && r.error ? r.error : "Invalid domain");
    }
  });
});

refresh();
setInterval(refresh, 1000);
