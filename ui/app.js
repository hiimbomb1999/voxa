"use strict";

const api = window.__TAURI__;
let isWindows = navigator.userAgent.includes("Windows");
const meetingCable = () => isWindows ? "VB-CABLE" : "BlackHole";
const credentialStore = () => isWindows ? "Windows Credential Manager" : "macOS Keychain";

const get = (id) => document.getElementById(id);
const invoke = (command, args) => api.core.invoke(command, args);
const activeStates = new Set(["opening", "connecting", "reconnecting", "ready", "speaking", "listening", "translating", "silent", "no_signal", "waiting_speech", "replying", "listen_quiet", "stopping"]);
const holdingStates = new Set(["ready", "speaking", "waiting_speech"]);
const laneLabels = { listen: "Chiều nghe", talk: "Chiều nói", assistant: "Trợ lý" };
const statuses = { listen: "stopped", talk: "stopped", assistant: "stopped" };
const latest = { original: "", translation: "" };
const displays = {
  listen: { start: "Bắt đầu dịch", stop: "Dừng dịch", idle: "Chưa bắt đầu", fields: ["source", "language", "speaker", "listen-voice", "duck-original"] },
  talk: { start: "Bật dịch micro", stop: "Tắt dịch micro", idle: "Chưa bật micro", fields: ["microphone", "blackhole", "talk-voice", "mic-sensitivity"] },
  assistant: { start: "Bật trợ lý", stop: "Tắt trợ lý", idle: "Phản hồi qua loa hoặc tai nghe.", fields: [] },
};
const voiceMotion = createVoiceMotion(get("voice-canvas"));
const levels = { listen: { peak: 0, outputPeak: 0 }, talk: { peak: 0, outputPeak: 0 } };
const voiceLabels = {
  opening: "Đang mở thiết bị",
  connecting: "Đang kết nối",
  reconnecting: "Đang kết nối lại",
  waiting_speech: "Chờ bạn nói",
  speaking: "Đang thu micro",
  replying: "Đang xử lý lượt nói",
  ready: "Giữ phím để nói",
  listening: "Đang nhận âm thanh",
  translating: "Đang dịch",
  silent: "Chờ âm thanh",
  listen_quiet: "Tạm tắt tiếng chiều nghe",
  no_signal: "Chưa có tín hiệu",
  stopping: "Đang dừng",
};
const languages = "af ak am ar hy az eu be bn bs bg my ca zh-Hans zh-Hant hr cs da nl en et fil fi fr gl ka de el gu ha he hi hu is id it ja jv kn kk km rw ko lo lv lt lb mk ms ml mt mr mn ne no fa pl pt-BR pt-PT ro ru sr si sk sl so es su sw sv ta te th tr uk ur uz vi zu";

let hasKey = false;
let keyChecked = false;
let hasSonioxKey = false;
let sonioxKeyChecked = false;
let checkingSoniox = false;
let sonioxLanguages = ["vi", "en"];
let checkingLive = false;
let refreshingDevices = false;
let refreshingVoices = false;
let soundTest = false;
let micTesting = false;
let returnToListen = false;
let currentSession;
let historyVersion = 0;
let historyListVersion = 0;
let historyBefore = null;
let deletingHistory = false;
const heard = () => activeStates.has(statuses.listen) || activeStates.has(statuses.talk);
const providerFor = (lane) => lane === "assistant" ? "gemini" : get("speech-provider").value;
const hasProviderKey = (provider) => provider === "soniox" ? hasSonioxKey : hasKey;
const providerKeyChecked = (provider) => provider === "soniox" ? sonioxKeyChecked : keyChecked;
const providerName = (provider) => provider === "soniox" ? "Soniox" : "Gemini";

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

const preferenceIds = ["speech-provider", "duck-original", "language", "microphone", "speaker", "blackhole", "talk-voice", "listen-voice", "mic-sensitivity", "quiet-listen", "save-history"];
let preferences = {};
try { preferences = JSON.parse(localStorage.getItem("voxa-audio-settings")) || {}; } catch {}

function restorePreference(id) {
  const element = get(id);
  const saved = preferences[id];
  if (element.type === "checkbox") {
    if (typeof saved === "boolean") element.checked = saved;
  } else if ([...element.options].some((option) => option.value === saved)) {
    element.value = saved;
  }
}

function savePreference(event) {
  const element = event.currentTarget;
  preferences[element.id] = element.type === "checkbox" ? element.checked : element.value;
  try { localStorage.setItem("voxa-audio-settings", JSON.stringify(preferences)); } catch {}
}

function replaceOptions(id, items, fallbackLabel, chooseFirst = false) {
  const select = get(id);
  const previous = select.value;
  const itemsToAdd = fallbackLabel ? [{ id: "", name: fallbackLabel }, ...items] : items;
  select.replaceChildren(...itemsToAdd.map(({ id, name }) => new Option(name, id)));
  if (itemsToAdd.some(({ id }) => id === previous)) select.value = previous;
  else if (chooseFirst && items.length) select.value = items[0].id;
  if (preferenceIds.includes(id)) restorePreference(id);
}

function fillLanguages() {
  const names = new Intl.DisplayNames(["vi"], { type: "language" });
  const available = providerFor("listen") === "soniox" ? sonioxLanguages : languages.split(" ");
  const others = available.filter((code) => !["vi", "en"].includes(code));
  const items = others.map((id) => ({ id, name: names.of(id) || id })).sort((a, b) => a.name.localeCompare(b.name, "vi"));
  replaceOptions("language", [{ id: "vi", name: "Tiếng Việt" }, { id: "en", name: "Tiếng Anh" }, ...items]);
}

function showView(id, focus = true) {
  for (const view of document.querySelectorAll(".view")) view.hidden = view.id !== id;
  for (const link of document.querySelectorAll(".nav")) {
    if (link.dataset.view === id) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  window.scrollTo(0, 0);
  if (focus) get(id + "-title").focus({ preventScroll: true });
  if (id === "history") void showHistory();
}

function openKeySettings(backToListen = false, provider = providerFor("listen")) {
  returnToListen = backToListen;
  showView("settings", false);
  const input = get(provider === "soniox" ? "soniox-key" : "key");
  input.focus();
  input.scrollIntoView({ block: "nearest" });
}

function applyPlatform() {
  for (const element of document.querySelectorAll("[data-mac-only]")) {
    element.hidden = isWindows;
  }
  get("test-loopback").textContent = `Nghe lại ${meetingCable()}`;
  get("loopback-help").textContent = `Hai tiếng bíp được thu từ ${meetingCable()} và phát lại qua loa hoặc tai nghe.`;
  get("latency-location").textContent = `Lượt gần nhất của mỗi giọng, đo đến ngõ ra ${meetingCable()}.`;
  get("meeting-microphone-help").textContent = isWindows
    ? "Trong Meet hoặc Zoom, chọn CABLE Output làm micro và tai nghe làm loa."
    : "Trong Meet hoặc Zoom, chọn BlackHole làm micro và tai nghe làm loa.";
}

function showMeetingCable(available, installed) {
  get("route-empty").hidden = available;
  get("talk-route").hidden = !available;
  get("route-state").textContent = installed ? `${meetingCable()} chưa sẵn sàng` : `Cần ${isWindows ? "VB-CABLE" : "BlackHole 2ch"}`;
  get("route-help").textContent = installed
    ? `Kiểm tra ${meetingCable()} trong cài đặt âm thanh rồi làm mới thiết bị.`
    : `Cài ${isWindows ? "VB-CABLE" : "BlackHole 2ch"} để người trong Meet hoặc Zoom nghe bản dịch của bạn.`;
}

async function refreshDevices() {
  if (!api || refreshingDevices) return;
  refreshingDevices = true;
  const buttons = [get("refresh"), get("routing-refresh")];
  for (const button of buttons) button.disabled = true;
  const voices = refreshVoices();
  try {
    const result = await invoke("devices");
    if (result.platform) isWindows = result.platform === "windows";
    applyPlatform();
    const named = (names) => names.map((name) => ({ id: name, name }));
    if (!activeStates.has(statuses.listen)) {
      replaceOptions("source", result.sources, "Toàn hệ thống");
      replaceOptions("speaker", named(result.speakers), result.default_output ? "Mặc định · " + result.default_output : "Mặc định hệ thống");
    }
    if (!activeStates.has(statuses.talk) && !activeStates.has(statuses.assistant)) {
      replaceOptions("microphone", named(result.microphones), result.default_microphone ? "Mặc định · " + result.default_microphone : "Mặc định hệ thống");
    }
    if (!activeStates.has(statuses.talk)) {
      replaceOptions("blackhole", named(result.blackhole), null, true);
      showMeetingCable(result.blackhole.length > 0, result.blackhole_installed);
    }
    message("route-warning", result.default_output_loops
      ? `Ngõ ra mặc định đang gửi qua ${meetingCable()}. Chọn tai nghe trong cài đặt hệ thống và trong Meet/Zoom.`
      : "");
    updateControls();
  } catch (error) {
    if (!get("route-empty").hidden) {
      get("route-state").textContent = "Không đọc được thiết bị";
      get("route-help").textContent = "Thử làm mới danh sách thiết bị âm thanh.";
    }
    message("message", errorText(error));
  } finally {
    refreshingDevices = false;
    for (const button of buttons) button.disabled = false;
    await voices;
  }
}

function paintKey() {
  const keyButton = get("key-status");
  const provider = providerFor("listen");
  const name = providerName(provider);
  const ready = hasProviderKey(provider);
  const checked = providerKeyChecked(provider);
  keyButton.dataset.ready = String(ready);
  keyButton.textContent = !api ? "Xem trước" : !checked ? `Đang kiểm tra ${name}` : ready ? `Đã lưu key ${name}` : `Thiết lập ${name}`;
  keyButton.disabled = !api || !checked;
  get("forget-key").disabled = !api || !hasKey;
  get("check-gemini").disabled = !api || !keyChecked || checkingLive;
  get("check-soniox").disabled = !api || checkingSoniox;
  get("forget-soniox").disabled = !api || (sonioxKeyChecked && !hasSonioxKey);
  for (const id of ["key", "soniox-key"]) get(id).disabled = !api;
  for (const id of ["key-form", "soniox-key-form"]) {
    get(id).querySelector("button[type=submit]").disabled = !api;
  }
  updateControls();
}

async function syncSpeechProvider() {
  fillLanguages();
  restorePreference("listen-voice");
  updateVoiceLanguage();
  paintKey();
  if (api && providerFor("listen") === "soniox" && !sonioxKeyChecked) {
    try {
      hasSonioxKey = await invoke("soniox_key_status");
    } catch (error) {
      message("message", errorText(error));
    } finally {
      sonioxKeyChecked = true;
      paintKey();
    }
  }
}

function localVoice(lane) {
  const value = get(lane + "-voice").value;
  return value.startsWith("vieneu:") ? value.slice("vieneu:".length) : null;
}

function updateVoiceLanguage() {
  const available = ["vi", "en"].includes(get("language").value);
  const soniox = providerFor("listen") === "soniox";
  const group = get("listen-local");
  const gemini = get("listen-gemini");
  gemini.disabled = gemini.hidden = soniox;
  const voice = get("listen-voice");
  if (soniox && voice.selectedOptions[0]?.parentElement === gemini) voice.value = "";
  voice.options[0].textContent = soniox ? "Adrian · Soniox" : "Trực tiếp · Độ trễ thấp";
  get("talk-voice").options[0].textContent = soniox ? "Adrian · Soniox" : "Trực tiếp · Gemini";
  group.hidden = !available || !group.children.length;
  group.disabled = !available;
  if (!available && localVoice("listen")) get("listen-voice").value = "";
  updateControls();
}

async function refreshVoices() {
  if (refreshingVoices) return;
  refreshingVoices = true;
  get("vieneu-refresh").disabled = true;
  message("vieneu-status", "Đang kiểm tra bộ giọng trên Mac…");
  try {
    const voices = await invoke("local_voices");
    for (const lane of ["listen", "talk"]) {
      const select = get(lane + "-voice");
      if (!activeStates.has(statuses[lane])) {
        const previous = select.value;
        get(lane + "-local").replaceChildren(...voices.map(([name, id]) =>
          new Option(name.replace(/^⭐\s*/, ""), "vieneu:" + id)));
        if ([...select.options].some((option) => option.value === previous)) select.value = previous;
        restorePreference(lane + "-voice");
      }
      get("vieneu-" + lane).replaceChildren(...[...select.children].map((option) => {
        const copy = option.cloneNode(true);
        copy.removeAttribute("id");
        return copy;
      }));
    }
    get("talk-voices").hidden = !get("talk-local").children.length;
    get("vieneu-settings").hidden = !voices.length;
    get("vieneu-missing").hidden = !!voices.length;
    message("vieneu-status", voices.length ? `Đã nhận ${voices.length} giọng trên Mac` : "Chưa tìm thấy bộ giọng VieNeu-TTS");
    updateVoiceLanguage();
  } catch (error) {
    message("vieneu-status", `Không đọc được bộ giọng trên Mac. ${errorText(error)}`);
  } finally {
    refreshingVoices = false;
    get("vieneu-refresh").disabled = false;
  }
}

async function testVoice(lane, meeting, status = meeting ? "meeting-test-result" : "message") {
  const voice = localVoice(lane);
  soundTest = true;
  updateControls();
  message(status, voice ? "Đang tạo câu mẫu bằng VieNeu-TTS…" : `Đang gửi âm thử qua ${meetingCable()}…`);
  try {
    await invoke("test_voice", {
      output: (meeting ? get("blackhole").value : get("speaker").value) || null,
      meeting,
      voice,
      language: lane === "talk" ? "en" : get("language").value,
    });
    message(status, meeting
      ? `Đã gửi âm thử qua ${meetingCable()}. Kiểm tra mức micro trong Meet hoặc nghe lại trong Zoom.`
      : "Đã phát câu mẫu qua loa hoặc tai nghe.");
  } catch (error) {
    message(status, errorText(error));
  } finally {
    soundTest = false;
    updateControls();
  }
}

async function testListenDucking() {
  soundTest = true;
  updateControls();
  message("duck-test-result", "Cứ để video phát. Tiếng gốc sẽ hạ xuống trong lúc có âm thử.");
  try {
    const result = await invoke("test_listen_ducking", {
      source: get("source").value || null,
      output: get("speaker").value || null,
    });
    message("duck-test-result", result.source_detected
      ? `Đã nhận tiếng gốc và phát âm thử qua ${result.output}.`
      : `Đã phát âm thử qua ${result.output}. Phát video ở nguồn đã chọn rồi thử lại để nghe mức giảm.`);
  } catch (error) {
    message("duck-test-result", errorText(error));
  } finally {
    soundTest = false;
    updateControls();
  }
}

async function testBlackHoleLoopback() {
  soundTest = true;
  updateControls();
  message("meeting-test-result", `Đang gửi và thu lại hai âm thử qua ${meetingCable()}…`);
  try {
    const { device, speaker } = await invoke("test_blackhole_loopback", {
      device: get("blackhole").value,
      speaker: get("speaker").value || null,
    });
    message("meeting-test-result", `Đã thu lại tín hiệu từ ${device} và phát qua ${speaker}.`);
  } catch (error) {
    message("meeting-test-result", errorText(error));
  } finally {
    soundTest = false;
    updateControls();
  }
}

const formatMillis = new Intl.NumberFormat("vi");
const millis = (value) => formatMillis.format(value) + " ms";

function showListenLatency({ captionMs, audioMs }) {
  const measurements = [];
  if (Number.isFinite(captionMs)) measurements.push(`Phụ đề ${millis(captionMs)}`);
  if (Number.isFinite(audioMs)) measurements.push(`Giọng dịch ${millis(audioMs)}`);
  message("listen-latency", measurements.length
    ? `Lần đầu từ lúc gửi âm gốc có tín hiệu · ${measurements.join(" · ")}`
    : "");
}

function showListenHealth({ inputDroppedMs, audioDroppedMs, skippedVoiceJobs, peakAudioQueueMs, peakVoiceWaitMs }) {
  const measurements = [];
  if (peakAudioQueueMs) measurements.push(`Hàng phát cao nhất ${millis(peakAudioQueueMs)}`);
  if (peakVoiceWaitMs) measurements.push(`Chờ đọc lâu nhất ${millis(peakVoiceWaitMs)}`);
  if (inputDroppedMs) measurements.push(`Âm gốc bỏ qua ${millis(inputDroppedMs)}`);
  if (audioDroppedMs) measurements.push(`Âm dịch bỏ qua ${millis(audioDroppedMs)}`);
  if (skippedVoiceJobs) measurements.push(`${skippedVoiceJobs} lượt đọc bị bỏ hoặc lỗi`);
  message("listen-health", measurements.join(" · "));
}

function showMeetingLatency({ engine, totalMs, textWaitMs, voiceAndOutputMs }) {
  const names = { gemini: "Gemini", soniox: "Soniox", vieneu: "VieNeu-TTS" };
  if (!Object.hasOwn(names, engine)) return;
  const result = get("latency-" + engine);
  result.textContent = millis(totalMs);
  result.removeAttribute("aria-label");
  message("latency-breakdown", textWaitMs === null ? ""
    : `${names[engine]} · chờ bản dịch ${millis(textWaitMs)} · tạo giọng và phát ${millis(voiceAndOutputMs)}`);
  get("latency-announcement").textContent = `${names[engine]} phát sau ${millis(totalMs)}.`;
}

function updateLevel(lane, peak, outputPeak = 0) {
  const meter = get(lane + "-meter");
  if (meter) meter.value = peak;
  if (!Object.hasOwn(levels, lane)) return;
  levels[lane] = { peak, outputPeak };
  renderVoiceStage();
}

function renderVoiceStage() {
  const talkState = statuses.talk;
  const listenState = statuses.listen;
  const talkActive = activeStates.has(talkState);
  const listenActive = activeStates.has(listenState);
  const showTalk = micTesting || talkActive && (talkState !== "ready" || !listenActive);
  const lane = showTalk ? "talk" : "listen";
  const state = statuses[lane];
  const { peak, outputPeak } = levels[lane];
  const active = showTalk ? talkActive : listenActive;
  const output = active && outputPeak > 0.003 && (lane === "listen" || state === "replying");
  const input = micTesting || active && (lane === "talk" ? holdingStates.has(state) && state !== "ready" : peak > 0.002);
  let mode = "idle";
  if (output) mode = "output";
  else if (input) mode = "input";
  else if (["opening", "connecting", "reconnecting", "replying", "translating"].includes(state)) mode = "work";

  const label = micTesting ? "Kiểm tra micro"
    : output ? (showTalk ? `Đang phát qua ${meetingCable()}` : "Đang phát bản dịch")
    : active ? voiceLabels[state]
    : [talkState, listenState].includes("error") ? "Phiên bị gián đoạn" : "Chưa bắt đầu";
  const channel = micTesting || showTalk ? (output ? get("blackhole").value : "Micro")
    : listenActive ? (output ? "Bản dịch" : "Âm thanh hệ thống") : "";
  if (get("voice-status").textContent !== label) get("voice-status").textContent = label;
  get("voice-channel").hidden = !channel;
  if (get("voice-channel").textContent !== channel) get("voice-channel").textContent = channel;
  get("voice-stage").dataset.mode = mode;
  const holdLabel = talkState !== "replying" ? "Giữ để nói" : output ? "Đợi phát xong" : "Đang xử lý lượt nói";
  if (get("talk-hold-label").textContent !== holdLabel) get("talk-hold-label").textContent = holdLabel;
  voiceMotion(mode, output ? outputPeak : input ? peak : 0);
}

function updateMeetingRoute() {
  const summary = get("talk-device-summary");
  const microphone = get("microphone").selectedOptions[0]?.textContent.replace(/^Mặc định · /, "");
  const output = get("blackhole").value;
  summary.hidden = !output;
  summary.textContent = microphone ? microphone + " → " + output : output;
}

async function testMicrophone() {
  soundTest = true;
  micTesting = true;
  updateControls();
  message("meeting-test-result", "Hãy nói một câu để xem mức micro.");
  try {
    const { name, peak } = await invoke("test_microphone", { microphone: get("microphone").value || null });
    if (peak >= 0.01) {
      const db = Math.round(20 * Math.log10(peak));
      message("meeting-test-result", `${name} · mức đỉnh ${db} dBFS`);
    } else {
      message("meeting-test-result", `Tín hiệu từ ${name} rất thấp. Chọn micro khác hoặc tăng mức đầu vào trong cài đặt âm thanh của hệ thống.`);
    }
  } catch (error) {
    message("meeting-test-result", errorText(error));
  } finally {
    micTesting = false;
    updateLevel("talk", 0);
    soundTest = false;
    updateControls();
  }
}

function updateControls() {
  for (const [lane, labels] of Object.entries(displays)) {
    const state = statuses[lane];
    const busy = activeStates.has(state);
    const toggle = get(lane + "-toggle");
    const provider = providerFor(lane);
    const keyAvailable = hasProviderKey(provider);
    const configure = api && providerKeyChecked(provider) && !keyAvailable && lane !== "assistant";
    toggle.textContent = state === "stopping" ? "Đang dừng…" : busy ? labels.stop : configure ? `Thiết lập ${providerName(provider)}` : labels.start;
    toggle.dataset.stop = String(busy);
    const conflict = lane === "talk" ? activeStates.has(statuses.assistant) : lane === "assistant" && activeStates.has(statuses.talk);
    toggle.disabled = state === "stopping" || (soundTest && !busy) || (!busy && ((!configure && (!api || !keyAvailable)) || conflict || (lane === "talk" && !get("blackhole").value)));
    for (const id of labels.fields) get(id).disabled = busy;
    const mic = get(lane + "-press");
    if (mic) {
      mic.disabled = !holdingStates.has(state);
      if (lane === "talk") {
        mic.hidden = mic.disabled && state !== "replying";
        get("voice-actions").hidden = mic.hidden;
      }
    }
  }
  get("microphone").disabled = activeStates.has(statuses.talk) || activeStates.has(statuses.assistant);
  const active = heard();
  get("speech-provider").disabled = active;
  get("talk-voices").hidden = providerFor("talk") !== "soniox" && !get("talk-local").children.length;
  const testingBlocked = soundTest || active || activeStates.has(statuses.assistant);
  for (const id of ["test-blackhole", "test-loopback"]) {
    get(id).disabled = testingBlocked || !get("blackhole").value;
  }
  get("test-microphone").disabled = testingBlocked;
  get("test-listen-ducking").disabled = !api || isWindows || testingBlocked;
  const language = get("language").value;
  const localLanguage = language === "vi" || language === "en";
  get("vieneu-language").value = localLanguage ? language : "";
  get("vieneu-language").disabled = activeStates.has(statuses.listen);
  for (const lane of ["listen", "talk"]) {
    const voice = get(lane + "-voice");
    const setting = get("vieneu-" + lane);
    setting.value = voice.value;
    setting.disabled = voice.disabled;
    if (setting.options.length) setting.options[0].textContent = voice.options[0].textContent;
    const gemini = setting.querySelector('optgroup[label="Gemini"]');
    if (gemini) gemini.disabled = gemini.hidden = providerFor(lane) === "soniox";
    const voiceGroup = setting.querySelector("optgroup:last-child");
    if (voiceGroup) {
      voiceGroup.hidden = lane === "listen" && !localLanguage;
      voiceGroup.disabled = voiceGroup.hidden;
    }
    const preview = get(lane + "-preview");
    preview.hidden = !localVoice(lane);
    preview.disabled = testingBlocked;
    get("vieneu-" + lane + "-preview").disabled = testingBlocked || !localVoice(lane);
  }
  get("empty-description").textContent = active ? "Đang chờ lời nói…" : "Phụ đề sẽ hiện khi nhận được lời nói.";
  updateMeetingRoute();
  renderVoiceStage();
  get("save-history").disabled = active || activeStates.has(statuses.assistant);
}

function applyStatus({ lane, status, detail }) {
  statuses[lane] = status;
  if (lane === "listen" && status === "ready") message("listen-route", `Phát qua ${detail}`);
  const label = get(lane + "-status");
  const card = get(lane + "-card");
  if (card) card.dataset.state = status;
  const hold = get(lane + "-press");
  if (hold) hold.dataset.pressed = String(status === "speaking" || status === "waiting_speech");
  if (!activeStates.has(status) || ["opening", "connecting", "reconnecting"].includes(status)) updateLevel(lane, 0);
  label.textContent = status === "stopped" ? displays[lane].idle : (detail || voiceLabels[status] || "Phiên bị gián đoạn");
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
  const provider = providerFor(lane);
  if (!activeStates.has(statuses[lane]) && !hasProviderKey(provider)) {
    openKeySettings(true, provider);
    return;
  }
  const destination = lane === "assistant" ? "settings-message" : "message";
  message(destination, "");
  if (activeStates.has(statuses[lane])) {
    applyStatus({ lane, status: "stopping", detail: "Đang dừng…" });
    await invoke("stop", { lane }).catch((error) => applyStatus({ lane, status: "error", detail: errorText(error) }));
    return;
  }
  if (lane === "listen") {
    message("listen-route", "");
    message("listen-latency", "");
    message("listen-health", "");
    message("duck-test-result", "");
  }
  if (lane !== "assistant" && !heard()) clearCaptions();
  if (lane === "assistant") get("assistant-lines").replaceChildren();
  applyStatus({ lane, status: "opening", detail: "Đang mở thiết bị âm thanh…" });
  const options = {
    lane,
    provider,
    source: get("source").value || null,
    language: get("language").value,
    microphone: get("microphone").value || null,
    output: (lane === "talk" ? get("blackhole").value : get("speaker").value) || null,
    sound: get("listen-sound").checked,
    duckOriginal: lane === "listen" && !isWindows && get("duck-original").checked,
    voice: lane === "assistant" ? null : (get(lane + "-voice").value || null),
    saveHistory: get("save-history").checked,
    micSensitivity: get("mic-sensitivity").value,
    quietListenDuringTalk: get("quiet-listen").checked,
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
  if (!caption.text.trim()) {
    line?.remove();
    if (isMain) {
      get("empty-caption").hidden = stream.querySelector(".line") !== null;
      if (caption.lane === "listen" || statuses.listen === "stopped" || statuses.listen === "error") {
        latest[caption.translation ? "translation" : "original"] = "";
        sendOverlay();
      }
    }
    return;
  }
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

const historyDate = new Intl.DateTimeFormat("vi", { dateStyle: "medium", timeStyle: "short" });
const isSelectedSession = (session) =>
  currentSession?.id === session.id && currentSession.started_at === session.started_at;

async function showHistory(before = null) {
  if (!api) return;
  const more = get("history-more");
  more.disabled = true;
  const version = ++historyListVersion;
  try {
    const { sessions, nextBefore } = await invoke("history_list", { before });
    if (version !== historyListVersion) return;
    const list = get("history-list");
    if (before === null) list.replaceChildren();
    historyBefore = nextBefore;
    more.hidden = nextBefore === null;
    if (!sessions.length && !list.children.length) {
      list.append(node("li", "Chưa có transcript đã lưu.", "placeholder"));
    }
    list.append(...sessions.map((session) => {
      const item = node("li", "", "history-row");
      const date = historyDate.format(session.started_at);
      const button = node("button", "", "history-select");
      button.dataset.id = session.id;
      if (isSelectedSession(session)) button.setAttribute("aria-current", "true");
      button.append(node("strong", session.preview), node("small", date + " · " + session.count + " dòng"));
      button.addEventListener("click", () => void selectSession(session));
      const remove = node("button", "Xóa", "history-delete danger");
      remove.setAttribute("aria-label", `Xóa phiên lúc ${date}`);
      remove.addEventListener("click", () => void deleteHistorySession(session));
      item.append(button, remove);
      return item;
    }));
  } catch (error) {
    if (version === historyListVersion) message("history-message", errorText(error));
  } finally {
    if (version === historyListVersion) more.disabled = false;
  }
}

async function selectSession(session) {
  const version = ++historyVersion;
  currentSession = session;
  const id = session.id;
  get("history-session-title").textContent = historyDate.format(session.started_at);
  get("summary").hidden = true;
  get("history-lines").replaceChildren();
  get("history-actions").hidden = true;
  for (const button of get("history-list").querySelectorAll(".history-select")) {
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

async function deleteHistorySession(session) {
  const dialog = get("history-delete-dialog");
  if (!session || deletingHistory || dialog.open) return;
  get("history-delete-preview").textContent = session.preview;
  dialog.returnValue = "";
  const confirmation = new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "delete"), { once: true });
  });
  dialog.showModal();
  if (!await confirmation) return;

  deletingHistory = true;
  try {
    await invoke("delete_session", { id: session.id, startedAt: session.started_at });
    if (isSelectedSession(session)) {
      currentSession = undefined;
      ++historyVersion;
      get("history-actions").hidden = true;
      get("history-session-title").textContent = "Chọn một phiên";
      get("history-lines").replaceChildren();
      get("summary").replaceChildren();
      get("summary").hidden = true;
    }
    message("history-message", "Đã xóa phiên.");
    const list = get("history-list");
    list.querySelector(`.history-select[data-id="${session.id}"]`)?.closest(".history-row")?.remove();
    if (!list.children.length) await showHistory(historyBefore);
    if (!get("history").hidden && document.activeElement === document.body) {
      (get("history-list").querySelector(".history-select") || get("reload-history")).focus({ preventScroll: true });
    }
  } catch (error) {
    message("history-message", errorText(error));
  } finally {
    deletingHistory = false;
  }
}

function bindHistory() {
  get("reload-history").addEventListener("click", () => void showHistory());
  get("history-more").addEventListener("click", () => void showHistory(historyBefore));
  get("export").addEventListener("click", async () => {
    if (!currentSession) return;
    const button = get("export");
    button.disabled = true;
    try {
      const result = await invoke("export_session", { id: currentSession.id, format: get("format").value });
      if (result.saved) message("history-message", "Đã xuất transcript.");
    } catch (error) { message("history-message", errorText(error)); }
    finally { button.disabled = false; }
  });
  get("summarize").addEventListener("click", async () => {
    if (!currentSession) return;
    const session = currentSession;
    const button = get("summarize");
    button.disabled = true;
    message("history-message", "Đang tóm tắt qua Gemini…");
    try {
      const text = await invoke("summarize_session", { id: session.id });
      if (!isSelectedSession(session)) return;
      get("summary").textContent = text;
      get("summary").hidden = false;
      message("history-message", "");
    } catch (error) { message("history-message", errorText(error)); }
    finally { button.disabled = false; }
  });
  get("delete").addEventListener("click", () => void deleteHistorySession(currentSession));
}

async function initialize() {
  if (api) document.body.classList.add("native-window");
  applyPlatform();
  fillLanguages();
  for (const id of preferenceIds) {
    restorePreference(id);
    get(id).addEventListener("change", savePreference);
  }
  for (const button of document.querySelectorAll("[data-view]")) {
    button.addEventListener("click", () => {
      returnToListen = false;
      showView(button.dataset.view);
    });
  }
  get("playback-toggle").addEventListener("click", () => {
    const button = get("playback-toggle");
    const show = button.getAttribute("aria-expanded") === "false";
    get("playback-options").hidden = !show;
    button.setAttribute("aria-expanded", String(show));
  });
  paintKey();
  if (!api) {
    get("overlay").disabled = true;
    get("refresh").disabled = true;
    get("routing-refresh").disabled = true;
    get("vieneu-refresh").disabled = true;
    message("vieneu-status", "Mở Voxa trên Mac để kiểm tra bộ giọng.");
    showMeetingCable(false, false);
    get("history-list").append(node("li", "Chưa có transcript đã lưu.", "placeholder"));
    return;
  }
  get("key-status").addEventListener("click", () => openKeySettings(false));
  get("speech-provider").addEventListener("change", () => void syncSpeechProvider());

  await Promise.all([
    api.event.listen("voxa:status", (event) => applyStatus(event.payload)),
    api.event.listen("voxa:caption", (event) => renderCaption(event.payload)),
    api.event.listen("voxa:microphone-test", ({ payload }) => updateLevel("talk", payload)),
    api.event.listen("voxa:listen-latency", ({ payload }) => showListenLatency(payload)),
    api.event.listen("voxa:listen-health", ({ payload }) => showListenHealth(payload)),
    api.event.listen("voxa:meeting-latency", ({ payload }) => showMeetingLatency(payload)),
    api.event.listen("voxa:signal", ({ payload }) => updateLevel(payload.lane, payload.peak, payload.outputPeak)),
    api.event.listen("voxa:notice", (event) => message("message", event.payload)),
    api.event.listen("voxa:overlay-ready", sendOverlay),
    api.event.listen("voxa:overlay-closed", () => get("overlay").setAttribute("aria-pressed", "false")),
  ]);

  for (const lane of Object.keys(displays)) get(lane + "-toggle").addEventListener("click", () => void toggleLane(lane));
  bindHold("talk");
  bindHold("assistant");
  bindHistory();
  get("refresh").addEventListener("click", refreshDevices);
  get("routing-refresh").addEventListener("click", refreshDevices);
  for (const id of ["microphone", "blackhole"]) get(id).addEventListener("change", updateMeetingRoute);
  get("test-microphone").addEventListener("click", () => void testMicrophone());
  get("quiet-listen").addEventListener("change", (event) => {
    void invoke("set_quiet_listen", { enabled: event.target.checked }).catch((error) => message("message", errorText(error)));
  });
  get("check-gemini").addEventListener("click", async () => {
    checkingLive = true;
    paintKey();
    message("settings-message", "Đang kiểm tra kết nối Gemini Live…");
    try {
      hasKey = await invoke("key_status");
      if (!hasKey) {
        message("settings-message", "Nhập Gemini API key rồi thử lại.");
        return;
      }
      await invoke("check_gemini_live");
      message("message", "");
      message("settings-message", "Gemini Live đã xác nhận cấu hình dịch.");
    } catch (error) {
      message("settings-message", errorText(error));
    } finally {
      checkingLive = false;
      paintKey();
    }
  });
  get("test-listen-ducking").addEventListener("click", () => void testListenDucking());
  get("test-loopback").addEventListener("click", () => void testBlackHoleLoopback());
  get("choose-monitor").addEventListener("click", () => {
    const button = get("playback-toggle");
    if (button.getAttribute("aria-expanded") === "false") button.click();
    get("speaker").focus();
  });
  get("test-blackhole").addEventListener("click", () => void testVoice("talk", true));
  get("vieneu-refresh").addEventListener("click", refreshVoices);
  get("vieneu-language").addEventListener("change", (event) => {
    get("language").value = event.target.value;
    get("language").dispatchEvent(new Event("change"));
  });
  get("language").addEventListener("change", updateVoiceLanguage);
  for (const lane of ["listen", "talk"]) {
    const voice = get(lane + "-voice");
    voice.addEventListener("change", updateControls);
    get("vieneu-" + lane).addEventListener("change", (event) => {
      voice.value = event.target.value;
      voice.dispatchEvent(new Event("change"));
      message("vieneu-preview-result", "");
    });
    get("vieneu-" + lane + "-preview").addEventListener("click", () => void testVoice(lane, false, "vieneu-preview-result"));
    get(lane + "-preview").addEventListener("click", () => void testVoice(lane, false));
  }
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
      if (returnToListen) {
        returnToListen = false;
        showView("live", false);
        get("listen-toggle").focus({ preventScroll: true });
        message("message", "Đã lưu API key. Bạn có thể bắt đầu dịch.");
      }
    } catch (error) { message("settings-message", errorText(error)); }
  });
  get("forget-key").addEventListener("click", async () => {
    if (!window.confirm("Xóa Gemini API key khỏi máy này?")) return;
    try {
      await invoke("forget_key");
      hasKey = false;
      paintKey();
      message("settings-message", `Đã xóa key khỏi ${credentialStore()}.`);
    } catch (error) { message("settings-message", errorText(error)); }
  });

  get("soniox-key-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    try {
      await invoke("store_soniox_key", { key: get("soniox-key").value.trim() });
      get("soniox-key").value = "";
      hasSonioxKey = sonioxKeyChecked = true;
      paintKey();
      message("settings-message", `Đã lưu Soniox key trong ${credentialStore()}.`);
      if (returnToListen) {
        returnToListen = false;
        showView("live", false);
        get("listen-toggle").focus({ preventScroll: true });
        message("message", "Đã lưu key Soniox. Bạn có thể bắt đầu dịch.");
      }
    } catch (error) { message("settings-message", errorText(error)); }
  });
  get("forget-soniox").addEventListener("click", async () => {
    try {
      await invoke("forget_soniox_key");
      hasSonioxKey = false;
      sonioxKeyChecked = true;
      paintKey();
      message("settings-message", `Đã xóa Soniox key khỏi ${credentialStore()}.`);
    } catch (error) { message("settings-message", errorText(error)); }
  });
  get("check-soniox").addEventListener("click", async () => {
    checkingSoniox = true;
    paintKey();
    message("settings-message", "Đang kiểm tra Soniox…");
    try {
      hasSonioxKey = await invoke("soniox_key_status");
      sonioxKeyChecked = true;
      if (!hasSonioxKey) {
        message("settings-message", "Nhập Soniox API key rồi thử lại.");
        return;
      }
      await invoke("check_soniox");
      message("settings-message", "Soniox đã xác nhận model stt-rt-v6 cho dự án.");
    } catch (error) { message("settings-message", errorText(error)); }
    finally { checkingSoniox = false; paintKey(); }
  });

  await Promise.all([
    refreshDevices(),
    invoke("soniox_languages")
      .then((codes) => { if (Array.isArray(codes) && codes.length) sonioxLanguages = codes; })
      .finally(syncSpeechProvider),
    invoke("key_status")
      .then((ready) => { hasKey = ready; })
      .catch((error) => message("message", errorText(error)))
      .finally(() => { keyChecked = true; paintKey(); }),
    invoke("shortcut_available").then((ready) => {
      if (!ready) get("talk-press").querySelector("kbd").textContent = "Phím tắt đang bận";
    }).catch(() => {}),
  ]);
}

initialize().catch((error) => message("message", errorText(error)));
