const glowSprites = new Map<string, HTMLCanvasElement>();

export function dotPulseDuration(): number {
  return 600 + Math.random() * 520;
}

export function dotPulseDelay(cadence: "active" | "ambient"): number {
  return cadence === "active"
    ? 120 + Math.random() * 147
    : 1200 + Math.random() * 1467;
}

export function drawDotPulse(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  baseRadius: number,
  progress: number,
  opacity = 1,
  ink = "255,255,255",
) {
  const rise = Math.min(1, progress * 8);
  const decay = Math.pow(1 - progress, 2.15);
  const intensity = rise * decay * opacity;
  const coreRadius = baseRadius * 1.05;
  ctx.globalAlpha = intensity;
  ctx.fillStyle = `rgb(${ink})`;
  ctx.beginPath();
  ctx.arc(x, y, coreRadius, 0, Math.PI * 2);
  ctx.fill();
  const haloRadius = baseRadius * (3.6 + progress * 8.4);
  ctx.globalAlpha = intensity * 0.64;
  ctx.drawImage(getGlowSprite(ink), x - haloRadius, y - haloRadius, haloRadius * 2, haloRadius * 2);
  ctx.globalAlpha = 1;
}

function getGlowSprite(ink: string): HTMLCanvasElement {
  const cached = glowSprites.get(ink);
  if (cached) return cached;
  const canvas = document.createElement("canvas");
  canvas.width = 96;
  canvas.height = 96;
  const context = canvas.getContext("2d");
  if (context) {
    const gradient = context.createRadialGradient(48, 48, 0, 48, 48, 48);
    gradient.addColorStop(0, `rgba(${ink},0.96)`);
    gradient.addColorStop(0.16, `rgba(${ink},0.62)`);
    gradient.addColorStop(0.52, `rgba(${ink},0.16)`);
    gradient.addColorStop(1, `rgba(${ink},0)`);
    context.fillStyle = gradient;
    context.fillRect(0, 0, 96, 96);
  }
  glowSprites.set(ink, canvas);
  return canvas;
}
