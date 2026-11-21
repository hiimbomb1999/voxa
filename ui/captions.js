"use strict";

const event = window.__TAURI__?.event;
const source = document.getElementById("overlay-original");
const target = document.getElementById("overlay-translation");

event?.listen("voxa:overlay-config", ({ payload }) => {
  source.hidden = payload.mode === "translation";
  target.hidden = payload.mode === "original";
  source.textContent = payload.original || (payload.mode === "original" ? "Đang chờ phụ đề từ Voxa" : "");
  target.textContent = payload.translation || "Đang chờ phụ đề từ Voxa";
}).then(() => event.emit("voxa:overlay-ready")).catch(() => {});
