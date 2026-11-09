import { useEffect, useRef } from "react";
import { shouldReduceMotion } from "../../appearance";
import { DARK_SCENE_PARTICLE_COLOR, readSceneParticlePalette } from "../../design-system/sceneParticlePalette";
import { dotPulseDelay, dotPulseDuration, drawDotPulse } from "./dotCloudPulse";
import { drawLockDotShader } from "./lockDotShader";

// Faithful port of the portfolio dot-cloud effect: organic (randomly sampled)
// point field on #000 with a golden-ratio reveal sweep, idle 3D wobble, and
// random brightness "pulses" (the twinkle). Extended here with per-dot color
// sampled from the source image so the provider marks glow in their own hues.

type Framing = "bottom" | "contain";

type Props = {
  src: string;
  framing?: Framing;
  motion?: "tilt" | "pan";
  onAssembled?: () => void;
  replayKey?: number;
  skipReveal?: boolean;
  className?: string;
};

type Dot = {
  x: number;
  y: number;
  z: number;
  b: number;
  r: number;
  g: number;
  bl: number;
  colored: boolean;
};

const SAMPLE_SIZE = 420;
const LUM_FLOOR = 0.1;
const REVEAL_SWEEP_MS = 1050;
const REVEAL_FADE_MS = 1350;
const MAX_RENDER_PIXEL_RATIO = 1.25;
const FRAME_INTERVAL_MS = 1000 / 30;

export function OctopusDotCloud({ src, framing = "bottom", motion = "tilt", onAssembled, replayKey = 0, skipReveal = false, className = "" }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const mouseRef = useRef({ x: 0, y: 0, active: false });
  const assembledRef = useRef(false);
  const onAssembledRef = useRef(onAssembled);
  onAssembledRef.current = onAssembled;

  useEffect(() => {
    let stopped = false;
    let rafId = 0;
    let isVisible = true;
    let inViewport = false;
    let dots: Dot[] | null = null;
    let brightIdx: number[] = [];
    let pulseList: Array<{ idx: number; t0: number; dur: number }> = [];
    let nextPulseAt = 0;
    let srcAspect = 1;
    let mx = 0;
    let my = 0;
    let revealStart: number | null = null;
    let wakeTimer = 0;
    let lastFrameAt = 0;
    let canvasWidth = canvasRef.current?.width ?? 0;
    let canvasHeight = canvasRef.current?.height ?? 0;
    let pixelRatio = Math.min(window.devicePixelRatio || 1, MAX_RENDER_PIXEL_RATIO);
    let settledPanCanvas: HTMLCanvasElement | null = null;

    const isHero = framing === "bottom";
    const target = isHero ? 13000 : 6400;
    const mouseWeight = isHero ? 0 : 0.36;
    const idleScale = isHero ? 0 : 0.5;
    const reduceMotion = shouldReduceMotion();
    let palette = readSceneParticlePalette();

    const spawnPulse = (now: number) => {
      if (brightIdx.length === 0) return;
      const idx = brightIdx[(Math.random() * brightIdx.length) | 0];
      pulseList.push({ idx, t0: now, dur: dotPulseDuration() });
    };

    const schedule = () => {
      if (stopped || rafId || !isVisible || !inViewport) return;
      if (wakeTimer) {
        window.clearTimeout(wakeTimer);
        wakeTimer = 0;
      }
      rafId = requestAnimationFrame(draw);
    };

    const wakeAt = (delay: number) => {
      if (stopped || wakeTimer || !isVisible || !inViewport) return;
      wakeTimer = window.setTimeout(() => {
        wakeTimer = 0;
        schedule();
      }, Math.max(FRAME_INTERVAL_MS, delay));
    };

    const resize = (width: number, height: number) => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      pixelRatio = Math.min(window.devicePixelRatio || 1, MAX_RENDER_PIXEL_RATIO);
      const nextWidth = Math.max(1, Math.floor(width * pixelRatio));
      const nextHeight = Math.max(1, Math.floor(height * pixelRatio));
      if (nextWidth === canvasWidth && nextHeight === canvasHeight) return;
      canvasWidth = nextWidth;
      canvasHeight = nextHeight;
      canvas.width = nextWidth;
      canvas.height = nextHeight;
      settledPanCanvas = null;
      schedule();
    };

    const draw = (now: number) => {
      rafId = 0;
      if (stopped) return;
      const canvas = canvasRef.current;
      if (!canvas || !dots || !isVisible || !inViewport) return;

      if (now - lastFrameAt < FRAME_INTERVAL_MS) {
        schedule();
        return;
      }
      lastFrameAt = now;

      const W = canvasWidth || canvas.width;
      const H = canvasHeight || canvas.height;
      const dpr = pixelRatio;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, W, H);

      const dstAspect = W / H;
      let drawW: number;
      let drawH: number;
      let offX: number;
      let offY: number;
      const fillW = isHero ? 0.99 : 0.9;
      if (srcAspect > dstAspect) {
        drawW = W * fillW;
        drawH = drawW / srcAspect;
      } else {
        drawH = H * (isHero ? 0.99 : 0.9);
        drawW = drawH * srcAspect;
      }
      offX = (W - drawW) / 2;
      offY = isHero ? H - drawH - 6 * dpr : (H - drawH) / 2;

      const tx = mouseRef.current.active ? mouseRef.current.x : 0;
      const ty = mouseRef.current.active ? mouseRef.current.y : 0;
      const follow = motion === "pan" ? 0.022 : 0.08;
      mx += (tx - mx) * follow;
      my += (ty - my) * follow;
      const t = now * 0.001;
      const elapsed = revealStart == null ? -1 : now - revealStart;

      const isPan = motion === "pan";
      const angY = isPan ? 0 : Math.sin(t * 0.22) * 0.05 * idleScale + mx * 0.22 * mouseWeight;
      const angX = isPan ? 0 : Math.sin(t * 0.15) * 0.03 * idleScale + my * -0.16 * mouseWeight;
      const panX = isPan ? mx * 4 * dpr : 0;
      const panY = isPan ? my * 3.2 * dpr : 0;
      const cosX = Math.cos(angX);
      const sinX = Math.sin(angX);
      const cosY = Math.cos(angY);
      const sinY = Math.sin(angY);
      const cxN = 0.5;
      const cyN = 0.5;
      const depthScale = 0.42;

      const revealDone = skipReveal || reduceMotion || elapsed > REVEAL_SWEEP_MS + REVEAL_FADE_MS;
      if (isPan && revealDone) {
        if (!settledPanCanvas) {
          settledPanCanvas = document.createElement("canvas");
          settledPanCanvas.width = W;
          settledPanCanvas.height = H;
          const settledContext = settledPanCanvas.getContext("2d");
          if (!settledContext) return;
          for (let i = 0; i < dots.length; i += 1) {
            const d = dots[i];
            drawLockDotShader(settledContext, {
              x: offX + d.x * drawW,
              y: offY + d.y * drawH,
              depth: 0.5 + d.z * 0.5,
              brightness: d.b,
              red: d.r,
              green: d.g,
              blue: d.bl,
              palette,
              dpr,
            });
          }
        }
        ctx.drawImage(settledPanCanvas, panX, panY);
      } else {
        for (let i = 0; i < dots.length; i += 1) {
          const d = dots[i];
          const revealA = skipReveal || reduceMotion ? 1 : revealAlpha(i, elapsed, REVEAL_SWEEP_MS, REVEAL_FADE_MS);
          if (revealA <= 0) continue;
          const nx = d.x - cxN;
          const ny = d.y - cyN;
          const nz = d.z * depthScale;
          const x1 = nx * cosY + nz * sinY;
          const z1 = -nx * sinY + nz * cosY;
          const y1 = ny * cosX - z1 * sinX;
          const z2 = ny * sinX + z1 * cosX;
          const px = offX + (cxN + x1) * drawW + panX;
          const py = offY + (cyN + y1) * drawH + panY;
          const depthT = 0.5 + (z2 / depthScale) * 0.5;
          const zVis = Math.max(0, Math.min(1, depthT));
          drawLockDotShader(ctx, {
            x: px,
            y: py,
            depth: zVis,
            brightness: d.b,
            red: d.r,
            green: d.g,
            blue: d.bl,
            palette,
            dpr,
            opacity: revealA,
          });
        }
      }

      if (revealDone && nextPulseAt === 0) {
        nextPulseAt = now + dotPulseDelay("ambient");
      }
      if (revealDone && now >= nextPulseAt) {
        spawnPulse(now);
        if (Math.random() < 0.08) spawnPulse(now);
        nextPulseAt = now + dotPulseDelay("ambient");
      }
      ctx.save();
      ctx.globalCompositeOperation = palette.darkSurface ? "lighter" : "source-over";
      const next: typeof pulseList = [];
      for (const p of pulseList) {
        const tt = (now - p.t0) / p.dur;
        if (tt >= 1) continue;
        const d = dots[p.idx];
        if (!d) continue;
        const nx = d.x - cxN;
        const ny = d.y - cyN;
        const nz = d.z * depthScale;
        const x1 = nx * cosY + nz * sinY;
        const z1 = -nx * sinY + nz * cosY;
        const y1 = ny * cosX - z1 * sinX;
        const px = offX + (cxN + x1) * drawW + panX;
        const py = offY + (cyN + y1) * drawH + panY;
        const baseR = (0.9 + d.z * 1.4) * dpr;
        drawDotPulse(ctx, px, py, baseR, tt, 1, palette.pulseInk);
        next.push(p);
      }
      pulseList = next;
      ctx.restore();

      if (!assembledRef.current && elapsed >= 0 && elapsed > REVEAL_SWEEP_MS + REVEAL_FADE_MS) {
        assembledRef.current = true;
        onAssembledRef.current?.();
      }

      const targetSettled = Math.abs(tx - mx) < 0.0005 && Math.abs(ty - my) < 0.0005;
      if (!isPan || !revealDone || !targetSettled || pulseList.length > 0) {
        schedule();
      } else {
        wakeAt(nextPulseAt - now);
      }
    };

    loadDots(src, target)
      .then((result) => {
        if (stopped) return;
        dots = result.dots;
        srcAspect = result.aspect;
        brightIdx = collectBrightDots(dots);
        if (skipReveal || reduceMotion) {
          assembledRef.current = true;
          onAssembledRef.current?.();
        }
        schedule();
      })
      .catch((error) => {
        if (stopped) return;
        throw new Error(`Cannot build Octopus dot cloud: ${error instanceof Error ? error.message : String(error)}`);
      });

    const canvasEl = canvasRef.current;
    let io: IntersectionObserver | null = null;
    let resizeObserver: ResizeObserver | null = null;
    if (canvasEl) {
      const rect = canvasEl.getBoundingClientRect();
      resize(rect.width, rect.height);
      resizeObserver = new ResizeObserver((entries) => {
        const entry = entries[entries.length - 1];
        if (entry) resize(entry.contentRect.width, entry.contentRect.height);
      });
      resizeObserver.observe(canvasEl);
      io = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            inViewport = entry.isIntersecting;
            if (entry.isIntersecting && revealStart == null) revealStart = performance.now();
            schedule();
          });
        },
        { threshold: 0.05 },
      );
      io.observe(canvasEl);
    }

    const onVisibility = () => {
      const nextVisible = !document.hidden;
      isVisible = nextVisible;
      if (!isVisible && rafId) {
        cancelAnimationFrame(rafId);
        rafId = 0;
      }
      if (!isVisible && wakeTimer) {
        window.clearTimeout(wakeTimer);
        wakeTimer = 0;
      }
      schedule();
    };

    const onMove = (event: MouseEvent) => {
      const c = canvasRef.current;
      if (!c) return;
      const r = c.getBoundingClientRect();
      const cx = r.left + r.width / 2;
      const cy = r.top + r.height / 2;
      mouseRef.current.x = Math.max(-1, Math.min(1, (event.clientX - cx) / (r.width * 0.5)));
      mouseRef.current.y = Math.max(-1, Math.min(1, (event.clientY - cy) / (r.height * 0.5)));
      mouseRef.current.active = true;
      schedule();
    };
    const onLeave = () => {
      mouseRef.current.active = false;
      schedule();
    };
    const allowMouse = window.matchMedia("(hover: hover) and (pointer: fine)").matches;
    if (allowMouse) {
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseleave", onLeave);
    }
    document.addEventListener("visibilitychange", onVisibility);
    onVisibility();

    const themeObserver = new MutationObserver(() => {
      palette = readSceneParticlePalette();
      settledPanCanvas = null;
      schedule();
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
      window.clearTimeout(wakeTimer);
      io?.disconnect();
      resizeObserver?.disconnect();
      themeObserver.disconnect();
      if (allowMouse) {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseleave", onLeave);
      }
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [src, framing, replayKey, motion, skipReveal]);

  return <canvas ref={canvasRef} className={`octopus-cloud ${className}`.trim()} aria-hidden="true" />;
}

function collectBrightDots(dots: Dot[]) {
  const indices: number[] = [];
  for (let i = 0; i < dots.length; i += 1) {
    const dot = dots[i];
    if (dot.colored || (dot.b > 0.55 && dot.z > 0.35)) indices.push(i);
  }
  return indices;
}

function loadDots(srcUrl: string, target: number): Promise<{ dots: Dot[]; aspect: number }> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(sampleRandom(image, target, seededRandom(hashText(srcUrl))));
    image.onerror = () => reject(new Error(`image failed to load: ${srcUrl}`));
    image.src = srcUrl;
  });
}

// Organic distribution via rejection sampling at continuous positions —
// no grid lattice, so it reads as a cloud, not an infographic.
function sampleRandom(image: HTMLImageElement, target: number, random: () => number): { dots: Dot[]; aspect: number } {
  const aspect = image.naturalWidth / Math.max(1, image.naturalHeight);
  const sw = aspect >= 1 ? SAMPLE_SIZE : Math.round(SAMPLE_SIZE * aspect);
  const sh = aspect >= 1 ? Math.round(SAMPLE_SIZE / aspect) : SAMPLE_SIZE;
  const canvas = document.createElement("canvas");
  canvas.width = sw;
  canvas.height = sh;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("2d context unavailable for sampling");
  ctx.clearRect(0, 0, sw, sh);
  ctx.drawImage(image, 0, 0, sw, sh);
  const data = ctx.getImageData(0, 0, sw, sh).data;

  const dots: Dot[] = [];
  const maxAttempts = target * 60;
  let attempts = 0;
  while (dots.length < target && attempts < maxAttempts) {
    attempts += 1;
    const fx = random();
    const fy = random();
    const px = Math.min(sw - 1, (fx * sw) | 0);
    const py = Math.min(sh - 1, (fy * sh) | 0);
    const i = (py * sw + px) * 4;
    const a = data[i + 3] / 255;
    if (a < 0.16) continue;
    const r = data[i];
    const g = data[i + 1];
    const b = data[i + 2];
    const rn = r / 255;
    const gn = g / 255;
    const bn = b / 255;
    const lum = rn * 0.2126 + gn * 0.7152 + bn * 0.0722;
    if (lum < LUM_FLOOR) continue;
    // brighter pixels accepted more readily → density tracks the form
    if (random() > 0.35 + lum * 0.65) continue;
    const max = Math.max(rn, gn, bn);
    const min = Math.min(rn, gn, bn);
    const sat = max <= 0.001 ? 0 : (max - min) / max;
    const colored = sat > 0.26 && max > 0.32;
    const cxN = fx - 0.5;
    const cyN = fy - 0.5;
    const dist = Math.sqrt(cxN * cxN + cyN * cyN);
    const z = Math.max(0.05, Math.min(0.95, 0.45 + (0.5 - dist) * 0.5 + (random() - 0.5) * 0.4));
    dots.push({
      x: fx,
      y: fy,
      z,
      b: Math.max(0.28, Math.min(1, a * 0.55 + lum * 0.45)),
      r: colored ? Math.min(255, r + 26) : DARK_SCENE_PARTICLE_COLOR[0],
      g: colored ? Math.min(255, g + 26) : DARK_SCENE_PARTICLE_COLOR[1],
      bl: colored ? Math.min(255, b + 26) : DARK_SCENE_PARTICLE_COLOR[2],
      colored,
    });
  }

  return { dots, aspect };
}

function hashText(value: string) {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

function seededRandom(seed: number) {
  let state = seed || 0x6d2b79f5;
  return () => {
    state += 0x6d2b79f5;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 0x1_0000_0000;
  };
}

function revealAlpha(i: number, elapsed: number, sweepMs: number, fadeMs: number) {
  if (elapsed < 0) return 0;
  const hash = (i * 0.61803398875) % 1;
  const t = (elapsed - hash * sweepMs) / fadeMs;
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  return t * t * (3 - 2 * t);
}
