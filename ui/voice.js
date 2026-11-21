"use strict";

function createVoiceMotion(canvas) {
  const context = canvas.getContext("2d");
  if (!context) return () => {};

  const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
  let width = 0;
  let height = 0;
  let frame = 0;
  let target = 0;
  let level = 0;
  let mode = "idle";

  function draw(time) {
    frame = 0;
    level = reducedMotion.matches ? target : level + (target - level) * 0.19;
    if (Math.abs(level - target) < 0.005) level = target;
    if (width < 1 || height < 1) return;

    context.clearRect(0, 0, width, height);
    const middle = height * 0.49;
    const busy = mode === "work";
    const amplitude = height * (0.038 + level * 0.37);
    const inks = mode === "output" ? ["#80e2ff", "#608dff", "#ad86fd"] :
      mode === "input" ? ["#f6a5df", "#be83ff", "#6a91ff"] : ["#d8aaff", "#9174f2", "#6c94d8"];
    const phase = reducedMotion.matches ? 0 : time / 340;

    for (let layer = 0; layer < inks.length; layer += 1) {
      const ink = inks[layer];
      const gradient = context.createLinearGradient(0, 0, width, 0);
      gradient.addColorStop(0, ink + "00");
      gradient.addColorStop(0.16, ink + "12");
      gradient.addColorStop(0.38, ink + "a6");
      gradient.addColorStop(0.61, ink + "f0");
      gradient.addColorStop(0.84, ink + "29");
      gradient.addColorStop(1, ink + "00");
      context.beginPath();
      for (let point = 0; point <= 80; point += 1) {
        const part = point / 80;
        const envelope = Math.sin(Math.PI * part) ** 3;
        const wave = Math.sin(part * (9 + layer * 2) + phase * 0.8 + layer * 1.75);
        const ripple = 0.22 * Math.sin(part * 21 - phase * 0.52 + layer);
        const y = middle + envelope * amplitude * (wave + ripple);
        if (point) context.lineTo(part * width, y);
        else context.moveTo(0, y);
      }
      context.lineWidth = layer === 0 ? 2.25 : 1.35;
      context.strokeStyle = gradient;
      context.shadowColor = ink;
      context.shadowBlur = layer === 0 ? 17 : 11;
      context.stroke();
      context.lineTo(width, middle);
      context.lineTo(0, middle);
      context.closePath();
      context.shadowBlur = 0;
      context.globalAlpha = 0.16;
      context.fillStyle = gradient;
      context.fill();
      context.globalAlpha = 1;
    }

    if (!reducedMotion.matches && (level > 0 || busy)) {
      frame = requestAnimationFrame(draw);
    }
  }

  function resize() {
    const rect = canvas.getBoundingClientRect();
    width = rect.width;
    height = rect.height;
    const scale = Math.min(devicePixelRatio || 1, 2);
    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
    context.setTransform(scale, 0, 0, scale, 0, 0);
    schedule();
  }

  function schedule() {
    if (!frame) frame = requestAnimationFrame(draw);
  }

  new ResizeObserver(resize).observe(canvas);
  reducedMotion.addEventListener("change", schedule);
  return (nextMode, peak) => {
    mode = nextMode;
    // The indicator uses the callback peak. The emphasis makes quieter voices visible.
    target = Math.min(1, Math.sqrt(Math.max(0, peak - 0.002)) * 1.6);
    schedule();
  };
}
