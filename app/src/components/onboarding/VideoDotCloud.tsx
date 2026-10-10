import { useEffect, useRef } from "react";
import { shouldReduceMotion } from "../../appearance";
import { dotPulseDelay, dotPulseDuration, drawDotPulse } from "./dotCloudPulse";

// A dot cloud driven by a <video>. Dot POSITIONS are sampled once from a
// still first-frame image (sampleSrc — the same PNG the morph cloud blends
// into, so the morph→video handoff is seamless); dot COLORS are read from the
// playing video every frame, so the cloud "plays" the video through points.
// Same reveal sweep + twinkle pulses as the rest of the onboarding. The loop
// seam blends toward the first frame's colors instead of dimming to black.

type Props = {
  src: string;
  sampleSrc: string;
  /** Scroll progress in icon units (0..n-1). The cloud fades in as progress
   *  crosses the last icon boundary, so it appears exactly when the morph
   *  cloud reaches the video's first frame. */
  progressRef: React.RefObject<number>;
  /** Progress value at which the video cloud becomes fully visible. */
  revealAt: number;
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

const SAMPLE_SIZE = 320;
const TARGET = 3500;
const LUM_FLOOR = 0.1;
const REVEAL_SWEEP_MS = 1050;
const REVEAL_FADE_MS = 1350;
const LOOP_BLEND_S = 0.7;
const DIM = 0.95;
const VIDEO_LUM_FLOOR = 34;
const VIDEO_LUM_FULL = 132;

export function VideoDotCloud({ src, sampleSrc, progressRef, revealAt, className = "" }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let stopped = false;
    let rafId = 0;
    let isVisible = true;
    let inViewport = false;
    let dots: Dot[] | null = null;
    let srcAspect = 1;
    let firstFrameData: ImageData | null = null;
    let brightIdx: number[] = [];
    let pulseList: Array<{ idx: number; t0: number; dur: number }> = [];
    let nextPulseAt = 0;
    let revealStart: number | null = null;

    const reduceMotion = shouldReduceMotion();
    const darkSurface = document.documentElement.dataset.theme !== "light";
    const pulseInk = darkSurface ? "255,255,255" : "38,40,44";

    const video = document.createElement("video");
    video.src = src;
    video.loop = true;
    video.muted = true;
    video.defaultMuted = true;
    video.playsInline = true;
    video.preload = "auto";
    video.autoplay = true;
    // Same-origin: no crossOrigin needed, and setting it can make the canvas
    // read fail if the dev server doesn't return CORS headers on the video.
    // Attach to DOM (hidden) — some webviews won't play a detached video.
    video.style.position = "fixed";
    video.style.left = "-9999px";
    video.style.width = "2px";
    video.style.height = "2px";
    video.style.opacity = "0";
    video.style.pointerEvents = "none";
    document.body.appendChild(video);
    void video.load();

    const sampleCanvas = document.createElement("canvas");
    const sampleCtx = sampleCanvas.getContext("2d", { willReadFrequently: true });

    const spawnPulse = (now: number) => {
      if (brightIdx.length === 0) return;
      const idx = brightIdx[(Math.random() * brightIdx.length) | 0];
      pulseList.push({ idx, t0: now, dur: dotPulseDuration() });
    };

    const schedule = () => {
      if (stopped || rafId || !isVisible || !inViewport) return;
      rafId = requestAnimationFrame(draw);
    };

    const draw = (now: number) => {
      rafId = 0;
      if (stopped) return;
      const canvas = canvasRef.current;
      if (!canvas || !dots || !isVisible || !inViewport || !sampleCtx) return;

      const rect = canvas.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const W = Math.max(1, Math.floor(rect.width * dpr));
      const H = Math.max(1, Math.floor(rect.height * dpr));
      if (canvas.width !== W || canvas.height !== H) {
        canvas.width = W;
        canvas.height = H;
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, W, H);

      // Pull the current video frame into the small sample buffer.
      const sw = sampleCanvas.width;
      const sh = sampleCanvas.height;
      let imgData: ImageData | null = null;
      if (sw > 0 && sh > 0) {
        try {
          sampleCtx.drawImage(video, 0, 0, sw, sh);
          imgData = sampleCtx.getImageData(0, 0, sw, sh);
        } catch {
          imgData = null; // video not ready or tainted; fall back to base color
        }
      }

      const dstAspect = W / H;
      let drawW: number;
      let drawH: number;
      let offX: number;
      let offY: number;
      const fillW = 0.92;
      if (srcAspect > dstAspect) {
        drawW = W * fillW;
        drawH = drawW / srcAspect;
      } else {
        drawH = H * 0.92;
        drawW = drawH * srcAspect;
      }
      offX = (W - drawW) / 2;
      offY = (H - drawH) / 2;

      const elapsed = revealStart == null ? -1 : now - revealStart;

      // Crossfade with the morph cloud as scroll reaches the video asset.
      const prog = progressRef.current || 0;
      const videoOpacity = smoothstep(revealAt - 1, revealAt, prog);
      if (videoOpacity <= 0.001) {
        schedule();
        return;
      }

      const dur = video.duration || 0;
      const ct = video.currentTime;
      const seamBlend =
        dur > 0 && !reduceMotion && firstFrameData && ct > dur - LOOP_BLEND_S
          ? smoothstep(dur - LOOP_BLEND_S, dur, ct)
          : 0;

      for (let i = 0; i < dots.length; i += 1) {
        const d = dots[i];
        const revealA = reduceMotion ? 1 : revealAlpha(i, elapsed, REVEAL_SWEEP_MS, REVEAL_FADE_MS);
        if (revealA <= 0) continue;
        let r = d.r;
        let g = d.g;
        let bl = d.bl;
        if (imgData) {
          const px = Math.min(sw - 1, (d.x * sw) | 0);
          const py = Math.min(sh - 1, (d.y * sh) | 0);
          const idx = (py * sw + px) * 4;
          r = imgData.data[idx];
          g = imgData.data[idx + 1];
          bl = imgData.data[idx + 2];
          if (firstFrameData && seamBlend > 0) {
            r = r + (firstFrameData.data[idx] - r) * seamBlend;
            g = g + (firstFrameData.data[idx + 1] - g) * seamBlend;
            bl = bl + (firstFrameData.data[idx + 2] - bl) * seamBlend;
          }
        }
        const videoLight = smoothstep(VIDEO_LUM_FLOOR, VIDEO_LUM_FULL, luminance(r, g, bl));
        if (videoLight <= 0.01) continue;
        const color = amplifyColor(r, g, bl);
        const px = offX + d.x * drawW;
        const py = offY + d.y * drawH;
        const size = (0.32 + d.z * 0.82) * (0.62 + d.b * 0.38) * dpr;
        const alpha = Math.min(
          0.96,
          (0.42 + d.z * 0.72) * (0.55 + d.b * 0.58) * revealA * DIM * videoOpacity * videoLight,
        );
        ctx.fillStyle = `rgba(${color.r},${color.g},${color.b},${alpha})`;
        ctx.beginPath();
        ctx.arc(px, py, size, 0, Math.PI * 2);
        ctx.fill();
      }

      // Twinkle — only after the reveal sweep completes.
      const revealDone = reduceMotion || elapsed > REVEAL_SWEEP_MS + REVEAL_FADE_MS;
      if (revealDone && nextPulseAt === 0) nextPulseAt = now + dotPulseDelay("active");
      if (revealDone && now >= nextPulseAt) {
        spawnPulse(now);
        if (Math.random() < 0.4) spawnPulse(now);
        nextPulseAt = now + dotPulseDelay("active");
      }
      ctx.save();
      ctx.globalCompositeOperation = darkSurface ? "lighter" : "source-over";
      const next: typeof pulseList = [];
      for (const p of pulseList) {
        const tt = (now - p.t0) / p.dur;
        if (tt >= 1) continue;
        const d = dots[p.idx];
        if (!d) continue;
        const px = offX + d.x * drawW;
        const py = offY + d.y * drawH;
        const baseR = (0.9 + d.z * 1.4) * dpr;
        drawDotPulse(ctx, px, py, baseR, tt, videoOpacity, pulseInk);
        next.push(p);
      }
      pulseList = next;
      ctx.restore();

      schedule();
    };

    loadDotsFromImage(sampleSrc, TARGET, darkSurface)
      .then((result) => {
        if (stopped) return;
        dots = result.dots;
        srcAspect = result.aspect;
        brightIdx = [];
        for (let i = 0; i < dots.length; i += 1) {
          const d = dots[i];
          if (d.colored || (d.b > 0.55 && d.z > 0.35)) brightIdx.push(i);
        }
        const aspect = srcAspect;
        const sw = aspect >= 1 ? SAMPLE_SIZE : Math.round(SAMPLE_SIZE * aspect);
        const sh = aspect >= 1 ? Math.round(SAMPLE_SIZE / aspect) : SAMPLE_SIZE;
        sampleCanvas.width = sw;
        sampleCanvas.height = sh;
        void loadFirstFrameData(sampleSrc, sw, sh)
          .then((data) => {
            firstFrameData = data;
            schedule();
          })
          .catch(() => {
            firstFrameData = null;
          });
        // Kick off playback as soon as dots are ready and the cloud is on
        // screen; opacity is gated by scroll progress in draw(), not by play
        // state, so the video is already rolling when it fades in.
        if (inViewport) {
          void video.play().catch(() => {});
        }
        schedule();
      })
      .catch((error) => {
        if (stopped) return;
        throw new Error(`Cannot build video dot cloud: ${error instanceof Error ? error.message : String(error)}`);
      });

    const canvasEl = canvasRef.current;
    let io: IntersectionObserver | null = null;
    if (canvasEl) {
      io = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            inViewport = entry.isIntersecting;
            if (entry.isIntersecting && revealStart == null) revealStart = performance.now();
            if (entry.isIntersecting) {
              void video.play().catch(() => {});
            } else {
              video.pause();
            }
            schedule();
          });
        },
        { threshold: 0.05 },
      );
      io.observe(canvasEl);
    }

    const onVisibility = () => {
      isVisible = !document.hidden;
      if (!isVisible) {
        video.pause();
        if (rafId) {
          cancelAnimationFrame(rafId);
          rafId = 0;
        }
      } else if (inViewport) {
        void video.play().catch(() => {});
      }
      schedule();
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
      io?.disconnect();
      video.pause();
      video.remove();
      video.src = "";
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [src, sampleSrc, progressRef, revealAt]);

  return <canvas ref={canvasRef} className={`octopus-cloud ${className}`.trim()} aria-hidden="true" />;
}

function loadDotsFromImage(srcUrl: string, target: number, darkSurface: boolean): Promise<{ dots: Dot[]; aspect: number }> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(sampleRandom(image, target, darkSurface));
    image.onerror = () => reject(new Error(`image failed to load: ${srcUrl}`));
    image.src = srcUrl;
  });
}

function loadFirstFrameData(
  srcUrl: string,
  width: number,
  height: number,
): Promise<ImageData> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d", { willReadFrequently: true });
      if (!ctx) {
        reject(new Error("2d context unavailable for first frame"));
        return;
      }
      ctx.clearRect(0, 0, width, height);
      ctx.drawImage(image, 0, 0, width, height);
      resolve(ctx.getImageData(0, 0, width, height));
    };
    image.onerror = () => reject(new Error(`first frame failed to load: ${srcUrl}`));
    image.src = srcUrl;
  });
}

// Organic distribution via rejection sampling at continuous positions —
// no grid lattice, so it reads as a cloud, not an infographic. Same logic as
// OctopusDotCloud so the morph cloud and the video cloud share one look.
function sampleRandom(image: HTMLImageElement, target: number, darkSurface: boolean): { dots: Dot[]; aspect: number } {
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
    const fx = Math.random();
    const fy = Math.random();
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
    if (Math.random() > 0.35 + lum * 0.65) continue;
    const max = Math.max(rn, gn, bn);
    const min = Math.min(rn, gn, bn);
    const sat = max <= 0.001 ? 0 : (max - min) / max;
    const colored = sat > 0.26 && max > 0.32;
    const cxN = fx - 0.5;
    const cyN = fy - 0.5;
    const dist = Math.sqrt(cxN * cxN + cyN * cyN);
    const z = Math.max(0.05, Math.min(0.95, 0.45 + (0.5 - dist) * 0.5 + (Math.random() - 0.5) * 0.4));
    dots.push({
      x: fx,
      y: fy,
      z,
      b: Math.max(0.28, Math.min(1, a * 0.55 + lum * 0.45)),
      r: colored ? Math.min(255, r + 26) : darkSurface ? 235 : 38,
      g: colored ? Math.min(255, g + 26) : darkSurface ? 238 : 40,
      bl: colored ? Math.min(255, b + 26) : darkSurface ? 244 : 44,
      colored,
    });
  }

  return { dots, aspect };
}

function revealAlpha(i: number, elapsed: number, sweepMs: number, fadeMs: number) {
  if (elapsed < 0) return 0;
  const hash = (i * 0.61803398875) % 1;
  const t = (elapsed - hash * sweepMs) / fadeMs;
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  return t * t * (3 - 2 * t);
}

function smoothstep(edge0: number, edge1: number, x: number) {
  const t = Math.max(0, Math.min(1, (x - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
}

function luminance(r: number, g: number, b: number): number {
  return r * 0.2126 + g * 0.7152 + b * 0.0722;
}

function amplifyColor(r: number, g: number, b: number): { r: number; g: number; b: number } {
  const avg = (r + g + b) / 3;
  const contrast = 1.16;
  const saturation = 1.62;
  const lift = 12;
  return {
    r: clampColor((avg + (r - avg) * saturation - 128) * contrast + 128 + lift),
    g: clampColor((avg + (g - avg) * saturation - 128) * contrast + 128 + lift),
    b: clampColor((avg + (b - avg) * saturation - 128) * contrast + 128 + lift),
  };
}

function clampColor(value: number): number {
  return Math.max(0, Math.min(255, Math.round(value)));
}
