"use strict";

const api = window.__TAURI__;
const get = (id) => document.getElementById(id);
const invoke = (command, args) => api.core.invoke(command, args);
const activeStates = new Set(["opening", "connecting", "reconnecting", "ready", "speaking", "listening", "translating", "silent", "no_signal", "stopping"]);
const holdingStates = new Set(["ready", "speaking"]);
const laneLabels = { listen: "Chiều nghe", talk: "Chiều nói", assistant: "Trợ lý" };
const statuses = { listen: "stopped", talk: "stopped", assistant: "stopped" };
const latest = { original: "", translation: "" };
const displays = {
  listen: { start: "Bắt đầu dịch", stop: "Dừng dịch", idle: "Chưa bắt đầu", fields: ["source", "language", "speaker", "listen-voice"] },
  talk: { start: "Bật dịch micro", stop: "Tắt dịch micro", idle: "Việt → Anh", fields: ["microphone", "blackhole"] },
  assistant: { start: "Bật trợ lý", stop: "Tắt trợ lý", idle: "Phản hồi qua loa hoặc tai nghe.", fields: [] },
};
const languages = "af ak am ar hy az eu be bn bs bg my ca zh-Hans zh-Hant hr cs da nl en et fil fi fr gl ka de el gu ha he hi hu is id it ja jv kn kk km rw ko lo lv lt lb mk ms ml mt mr mn ne no fa pl pt-BR pt-PT ro ru sr si sk sl so es su sw sv ta te th tr uk ur uz vi zu";

let hasKey = false;
let currentSession;
let historyVersion = 0;
const heard = () => activeStates.has(statuses.listen) || activeStates.has(statuses.talk);
const controlsBlocked = () => !api || !hasKey;

function message(id, text) {
  const node = get(id);
  node.textContent = text || "";
  node.hidden = !text;
}

function errorText(error) {
  return typeof error === "string" ? error : (error?.message || "Voxa không hoàn tất thao tác.");
}

function node(tag, text, className) {
  const element = document.createElement(tag);
  if (text) element.textContent = text;
  if (className) element.className = className;
  return element;
}

function replaceOptions(id, items, fallbackLabel, chooseFirst = false) {
  const select = get(id);
  const previous = select.value;
  const itemsToAdd = fallbackLabel ? [{ id: "", name: fallbackLabel }, ...items] : items;
  select.replaceChildren(...itemsToAdd.map(({ id, name }) => new Option(name, id)));
  if (itemsToAdd.some(({ id }) => id === previous)) select.value = previous;
  else if (chooseFirst && items.length) select.value = items[0].id;
}

function fillLanguages() {
  const names = new Intl.DisplayNames(["vi"], { type: "language" });
  const others = languages.split(" ").filter((code) => !["vi", "en"].includes(code));
  const items = others.map((id) => ({ id, name: names.of(id) || id })).sort((a, b) => a.name.localeCompare(b.name, "vi"));
  replaceOptions("language", [{ id: "vi", name: "Tiếng Việt" }, { id: "en", name: "Tiếng Anh" }, ...items]);
}

async function refreshDevices() {
  if (!api) return;
  try {
    const result = await invoke("devices");
    const named = (names) => names.map((name) => ({ id: name, name }));
    if (!activeStates.has(statuses.listen)) {
      replaceOptions("source", result.sources, "Toàn hệ thống");
      replaceOptions("speaker", named(result.speakers), "Mặc định của Mac");
    }
    if (!activeStates.has(statuses.talk) && !activeStates.has(statuses.assistant)) {
      replaceOptions("microphone", named(result.microphones), "Mặc định của Mac");
    }
    if (!activeStates.has(statuses.talk)) {
      replaceOptions("blackhole", named(result.blackhole), result.blackhole.length ? null : "Chưa tìm thấy BlackHole", true);
      get("routing-hint").textContent = result.blackhole.length
        ? "Trong Meet hoặc Zoom, chọn đúng thiết bị BlackHole này làm micro."
        : "Cài BlackHole 2ch, làm mới thiết bị, rồi chọn BlackHole làm micro trong Meet hoặc Zoom.";
    }
    updateControls();
  } catch (error) {
    message("message", errorText(error));
  }
}

function paintKey() {
  get("key-status").dataset.ready = String(hasKey);
  get("key-status").textContent = !api ? "Xem trước" : (hasKey ? "Đã lưu API key" : "Chưa có API key");
  get("setup-banner").hidden = !api || hasKey;
  get("forget-key").disabled = controlsBlocked();
  get("key").disabled = !api;
  get("key-form").querySelector("button[type=submit]").disabled = !api;
  updateControls();
}

function updateControls() {
  for (const [lane, labels] of Object.entries(displays)) {
    const state = statuses[lane];
    const busy = activeStates.has(state);
    const toggle = get(lane + "-toggle");
    toggle.textContent = state === "stopping" ? "Đang dừng…" : (busy ? labels.stop : labels.start);
    const conflict = lane === "talk" ? activeStates.has(statuses.assistant) : lane === "assistant" && activeStates.has(statuses.talk);
    toggle.disabled = state === "stopping" || (!busy && (controlsBlocked() || conflict || (lane === "talk" && !get("blackhole").value)));
    for (const id of labels.fields) get(id).disabled = busy;
    const mic = get(lane + "-press");
    if (mic) mic.disabled = !holdingStates.has(state);
  }
  get("microphone").disabled = activeStates.has(statuses.talk) || activeStates.has(statuses.assistant);
  const routeTest = get("test-blackhole");
  routeTest.hidden = !get("blackhole").value;
  routeTest.disabled = activeStates.has(statuses.talk);
  get("caption-state").textContent = heard() ? "Đang theo dõi phiên dịch" : "Chưa có phiên đang chạy";
  get("save-history").disabled = heard() || activeStates.has(statuses.assistant);
}

function applyStatus({ lane, status, detail }) {
  statuses[lane] = status;
  const label = get(lane + "-status");
  const hold = get(lane + "-press");
  if (hold) hold.dataset.pressed = String(status === "speaking");
  if (!activeStates.has(status)) get(lane + "-meter")?.setAttribute("value", "0");
  label.textContent = status === "stopped" ? displays[lane].idle : (detail || status);
  if (status === "error") message(lane === "assistant" ? "settings-message" : "message", detail);
  updateControls();
}

function clearCaptions() {
  for (const line of get("lines").querySelectorAll("article")) line.remove();
  get("empty-caption").hidden = false;
  latest.original = "";
  latest.translation = "";
  sendOverlay();
}

async function toggleLane(lane) {
  const destination = lane === "assistant" ? "settings-message" : "message";
  message(destination, "");
  if (activeStates.has(statuses[lane])) {
    applyStatus({ lane, status: "stopping", detail: "Đang dừng…" });
    await invoke("stop", { lane }).catch((error) => applyStatus({ lane, status: "error", detail: errorText(error) }));
    return;
  }
  if (lane !== "assistant" && !heard()) clearCaptions();
  if (lane === "assistant") get("assistant-lines").replaceChildren();
  applyStatus({ lane, status: "opening", detail: "Đang mở thiết bị âm thanh…" });
  const options = {
    lane,
    source: get("source").value || null,
    language: get("language").value,
    microphone: get("microphone").value || null,
    output: (lane === "talk" ? get("blackhole").value : get("speaker").value) || null,
    sound: get("listen-sound").checked,
    voice: lane === "listen" ? (get("listen-voice").value || null) : null,
    saveHistory: get("save-history").checked,
  };
  await invoke("start", { options }).catch((error) => applyStatus({ lane, status: "error", detail: errorText(error) }));
}

function bindHold(lane) {
  const button = get(lane + "-press");
  const set = (pressed) => invoke("press", { lane, pressed }).catch(() => {});
  const release = () => { if (held) { held = false; set(false); } };
  let held = false;
  button.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || button.disabled) return;
    button.setPointerCapture(event.pointerId);
    held = true;
    set(true);
  });
  for (const name of ["pointerup", "pointercancel", "lostpointercapture", "blur"]) button.addEventListener(name, release);
  button.addEventListener("keydown", (event) => {
    if (event.code !== "Space" && event.code !== "Enter") return;
    event.preventDefault();
    if (event.repeat || held) return;
    held = true;
    set(true);
  });
  button.addEventListener("keyup", (event) => {
    if (event.code === "Space" || event.code === "Enter") release();
  });
  window.addEventListener("blur", release);
}

function makeLine(caption) {
  const line = node("article", "", "line");
  line.append(node("small"), node("p"));
  populateLine(line, caption);
  return line;
}

function populateLine(line, caption) {
  const role = caption.translation ? (caption.lane === "assistant" ? "Phản hồi" : "Bản dịch") : "Lời gốc";
  line.className = "line " + (caption.translation ? "translation " : "") + caption.lane + (caption.finished === false ? " partial" : "");
  line.dataset.translation = String(caption.translation);
  line.firstElementChild.textContent = (laneLabels[caption.lane] || "Phiên dịch") + " · " + role;
  line.lastElementChild.textContent = caption.text;
  return line;
}

function filterCaption(line) {
  const mode = get("caption-mode").value;
  line.hidden = mode !== "both" && (mode === "translation") !== (line.dataset.translation === "true");
}

function renderCaption(caption) {
  const isMain = caption.lane !== "assistant";
  const stream = get(isMain ? "lines" : "assistant-lines");
  const stick = stream.scrollHeight - stream.scrollTop - stream.clientHeight < 64;
  let line = get("caption-" + caption.id);
  if (line) populateLine(line, caption);
  else {
    line = makeLine(caption);
    line.id = "caption-" + caption.id;
    stream.append(line);
    const limit = isMain ? 181 : 80;
    while (stream.children.length > limit) stream.children[isMain ? 1 : 0].remove();
  }
  if (isMain) {
    get("empty-caption").hidden = true;
    filterCaption(line);
    if (caption.lane === "listen" || statuses.listen === "stopped" || statuses.listen === "error") {
      latest[caption.translation ? "translation" : "original"] = caption.text;
      sendOverlay();
    }
  }
  if (stick) stream.scrollTop = stream.scrollHeight;
}

function sendOverlay() {
  if (!api || get("overlay").getAttribute("aria-pressed") !== "true") return;
  void api.event.emitTo("captions", "voxa:overlay-config", { mode: get("caption-mode").value, ...latest }).catch(() => {});
}

async function showHistory() {
  if (!api) return;
  try {
    const sessions = await invoke("history_list");
    const list = get("history-list");
    if (!sessions.length) {
      list.replaceChildren(node("li", "Chưa có transcript đã lưu.", "placeholder"));
      return;
    }
    const date = new Intl.DateTimeFormat("vi", { dateStyle: "medium", timeStyle: "short" });
    list.replaceChildren(...sessions.map((session) => {
      const item = node("li");
      const button = node("button");
      button.dataset.id = session.id;
      if (currentSession === session.id) button.setAttribute("aria-current", "true");
      button.append(node("strong", session.preview), node("small", date.format(session.started_at) + " · " + session.count + " dòng"));
      button.addEventListener("click", () => void selectSession(session.id, date.format(session.started_at)));
      item.append(button);
      return item;
    }));
  } catch (error) {
    message("history-message", errorText(error));
  }
}

async function selectSession(id, title) {
  const version = ++historyVersion;
  currentSession = id;
  get("history-session-title").textContent = title;
  get("summary").hidden = true;
  get("history-lines").replaceChildren();
  get("history-actions").hidden = true;
  for (const button of get("history-list").querySelectorAll("button")) {
    if (Number(button.dataset.id) === id) button.setAttribute("aria-current", "true");
    else button.removeAttribute("aria-current");
  }
  try {
    const entries = await invoke("history_entries", { id });
    if (version !== historyVersion) return;
    const panel = get("history-lines");
    for (const entry of entries) panel.append(makeLine(entry));
    get("history-actions").hidden = !entries.length;
  } catch (error) {
    if (version === historyVersion) message("history-message", errorText(error));
  }
}

function bindHistory() {
  get("reload-history").addEventListener("click", showHistory);
  get("export").addEventListener("click", async () => {
    if (!currentSession) return;
    const button = get("export");
    button.disabled = true;
    try {
      const result = await invoke("export_session", { id: currentSession, format: get("format").value });
      if (result.saved) message("history-message", "Đã xuất transcript.");
    } catch (error) { message("history-message", errorText(error)); }
    finally { button.disabled = false; }
  });
  get("summarize").addEventListener("click", async () => {
    if (!currentSession) return;
    const id = currentSession;
    const button = get("summarize");
    button.disabled = true;
    message("history-message", "Đang tóm tắt qua Gemini…");
    try {
      const text = await invoke("summarize_session", { id });
      if (currentSession !== id) return;
      get("summary").textContent = text;
      get("summary").hidden = false;
      message("history-message", "");
    } catch (error) { message("history-message", errorText(error)); }
    finally { button.disabled = false; }
  });
  get("delete").addEventListener("click", async () => {
    if (!currentSession || !window.confirm("Xóa transcript đã lưu của phiên này?")) return;
    try {
      await invoke("delete_session", { id: currentSession });
      currentSession = undefined;
      ++historyVersion;
      get("history-actions").hidden = true;
      get("history-session-title").textContent = "Chọn một phiên";
      get("history-lines").replaceChildren();
      get("summary").hidden = true;
      await showHistory();
    } catch (error) { message("history-message", errorText(error)); }
  });
}

async function initialize() {
  fillLanguages();
  for (const button of document.querySelectorAll("[data-view]")) {
    button.addEventListener("click", () => {
      const id = button.dataset.view;
      for (const view of document.querySelectorAll(".view")) view.hidden = view.id !== id;
      for (const link of document.querySelectorAll(".nav")) {
        if (link.dataset.view === id) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
      }
      if (id === "history") void showHistory();
    });
  }
  paintKey();
  if (!api) {
    get("overlay").disabled = true;
    get("refresh").disabled = true;
    message("message", "Mở bản .app trên Mac để thu âm thanh và dùng Gemini.");
    get("history-list").append(node("li", "Chưa có transcript đã lưu.", "placeholder"));
    return;
  }

  await Promise.all([
    api.event.listen("voxa:status", (event) => applyStatus(event.payload)),
    api.event.listen("voxa:caption", (event) => renderCaption(event.payload)),
    api.event.listen("voxa:signal", ({ payload }) => get(payload.lane + "-meter")?.setAttribute("value", String(payload.peak))),
    api.event.listen("voxa:notice", (event) => message("message", event.payload)),
    api.event.listen("voxa:overlay-ready", sendOverlay),
    api.event.listen("voxa:overlay-closed", () => get("overlay").setAttribute("aria-pressed", "false")),
  ]);

  for (const lane of Object.keys(displays)) get(lane + "-toggle").addEventListener("click", () => void toggleLane(lane));
  bindHold("talk");
  bindHold("assistant");
  bindHistory();
  get("refresh").addEventListener("click", refreshDevices);
  get("test-blackhole").addEventListener("click", async (event) => {
    const button = event.currentTarget;
    button.disabled = true;
    try {
      await invoke("test_blackhole", { output: get("blackhole").value });
      message("message", "Đã phát âm thử vào BlackHole. Kiểm tra mức micro trong Meet hoặc Zoom.");
    } catch (error) { message("message", errorText(error)); }
    finally { updateControls(); }
  });
  get("caption-mode").addEventListener("change", () => {
    for (const line of get("lines").querySelectorAll(".line")) filterCaption(line);
    sendOverlay();
  });
  get("listen-sound").addEventListener("change", (event) => {
    void invoke("set_listen_sound", { enabled: event.target.checked }).catch((error) => message("message", errorText(error)));
  });
  get("overlay").addEventListener("click", async () => {
    const button = get("overlay");
    const show = button.getAttribute("aria-pressed") !== "true";
    button.setAttribute("aria-pressed", String(show));
    try {
      await invoke("overlay", { show });
      sendOverlay();
    } catch (error) {
      button.setAttribute("aria-pressed", String(!show));
      message("message", errorText(error));
    }
  });
  get("key-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    try {
      await invoke("store_key", { key: get("key").value.trim() });
      get("key").value = "";
      hasKey = true;
      paintKey();
      message("settings-message", "Đã lưu key vào macOS Keychain.");
    } catch (error) { message("settings-message", errorText(error)); }
  });
  get("forget-key").addEventListener("click", async () => {
    if (!window.confirm("Xóa Gemini API key khỏi Mac này?")) return;
    try {
      await invoke("forget_key");
      hasKey = false;
      paintKey();
      message("settings-message", "Đã xóa key khỏi Keychain.");
    } catch (error) { message("settings-message", errorText(error)); }
  });

  await Promise.all([
    refreshDevices(),
    invoke("key_status").then((ready) => { hasKey = ready; paintKey(); }).catch((error) => message("message", errorText(error))),
    invoke("shortcut_available").then((ready) => {
      if (!ready) get("talk-press").querySelector("kbd").textContent = "Phím tắt đang bận";
    }).catch(() => {}),
  ]);
}

initialize().catch((error) => message("message", errorText(error)));
