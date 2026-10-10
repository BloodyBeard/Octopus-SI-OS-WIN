import { useEffect, useRef } from "react";
import { shouldReduceMotion } from "../../appearance";
import { readSceneParticlePalette, sceneParticleColor } from "../../design-system/sceneParticlePalette";
import { dotPulseDelay, dotPulseDuration, drawDotPulse } from "./dotCloudPulse";
import { drawLockDotShader } from "./lockDotShader";

type Props = {
  srcs: string[];
  videoSrc?: string;
  progressRef: React.RefObject<number>;
  frameScales?: readonly number[];
  particleScale?: number;
  monochrome?: boolean;
  className?: string;
};

type StateMorphProps = {
  srcs: string[];
  active: boolean;
  durationMs?: number;
  frameScales?: readonly number[];
  className?: string;
};

type CloudFrame = {
  x: Float32Array;
  y: Float32Array;
  z: Float32Array;
  r: Float32Array;
  g: Float32Array;
  b: Float32Array;
  weight: Float32Array;
  bright: Uint8Array;
};

type VideoTargets = {
  count: number;
  x: Float32Array;
  y: Float32Array;
  r: Uint8ClampedArray;
  g: Uint8ClampedArray;
  b: Uint8ClampedArray;
  gate: Float32Array;
};

const COUNT = 3500;
const SAMPLE_SIZE = 320;
const LUM_FLOOR = 0.1;
const FADE_MS = 700;
const LOOP_BLEND_S = 0.65;
const VIDEO_ENTRY_OFFSET_S = 0.45;
const LOOP_RESET_EPSILON_S = 0.05;
const VIDEO_PREROLL_PROGRESS = 0.35;
const VIDEO_SCROLL_ADVANCE_S = 1.4;
const VIDEO_LUM_FLOOR = 34;
const VIDEO_LUM_FULL = 132;
const DIM = 0.96;

export function StateMorphDotCloud({ srcs, active, durationMs = 720, frameScales, className = "" }: StateMorphProps) {
  const progressRef = useRef(active ? Math.max(0, srcs.length - 1) : 0);

  useEffect(() => {
    const target = active ? Math.max(0, srcs.length - 1) : 0;
    const from = progressRef.current;
    if (shouldReduceMotion() || from === target) {
      progressRef.current = target;
      return;
    }
    let rafId = 0;
    let startedAt: number | null = null;
    const animate = (now: number) => {
      if (startedAt === null) startedAt = now;
      const progress = Math.min(1, (now - startedAt) / Math.max(1, durationMs));
      progressRef.current = from + (target - from) * progress;
      if (progress < 1) rafId = requestAnimationFrame(animate);
    };
    rafId = requestAnimationFrame(animate);
    return () => cancelAnimationFrame(rafId);
  }, [active, durationMs, srcs.length]);

  return (
    <MorphVideoDotCloud
      srcs={srcs}
      progressRef={progressRef}
      frameScales={frameScales}
      className={className}
    />
  );
}

export function MorphVideoDotCloud({ srcs, videoSrc, progressRef, frameScales, particleScale = 1, monochrome = false, className = "" }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let stopped = false;
    let rafId = 0;
    let isVisible = true;
    let inViewport = false;
    let frames: CloudFrame[] | null = null;
    let videoFrameData: ImageData | null = null;
    let firstFrameData: ImageData | null = null;
    let sampleW = 0;
    let sampleH = 0;
    let srcAspect = 1;
    let videoFitX = 0.5;
    let videoFitY = 0.5;
    let startTime: number | null = null;
    let pulseList: Array<{ idx: number; t0: number; dur: number }> = [];
    let pulseIdx: number[] = [];
    let nextPulseAt = 0;
    let pulseCadence: "active" | "ambient" | null = null;
    let isInVideoSegment = false;
    const videoSeedU = new Float32Array(COUNT);
    const videoSeedV = new Float32Array(COUNT);
    const videoSeedA = new Float32Array(COUNT);
    for (let i = 0; i < COUNT; i += 1) {
      videoSeedU[i] = hash01(i, 17);
      videoSeedV[i] = hash01(i, 43);
      videoSeedA[i] = hash01(i, 71) * Math.PI * 2;
    }

    const canvas = canvasRef.current;
    if (!canvas) return;
    const reduceMotion = shouldReduceMotion();
    const video = videoSrc ? document.createElement("video") : null;
    if (video && videoSrc) {
      video.src = videoSrc;
      video.loop = false;
      video.muted = true;
      video.defaultMuted = true;
      video.playsInline = true;
      video.preload = "auto";
      video.style.position = "fixed";
      video.style.left = "-9999px";
      video.style.width = "2px";
      video.style.height = "2px";
      video.style.opacity = "0";
      video.style.pointerEvents = "none";
      document.body.appendChild(video);
      void video.load();
    }

    const sampleCanvas = document.createElement("canvas");
    const sampleCtx = sampleCanvas.getContext("2d", { willReadFrequently: true });

    const spawnPulse = (now: number) => {
      if (pulseIdx.length === 0) return;
      pulseList.push({ idx: pulseIdx[(Math.random() * pulseIdx.length) | 0], t0: now, dur: dotPulseDuration() });
    };

    const schedule = () => {
      if (stopped || rafId || !isVisible || !inViewport) return;
      rafId = requestAnimationFrame(draw);
    };

    const draw = (now: number) => {
      rafId = 0;
      if (stopped || !frames || (video && !sampleCtx)) return;
      if (startTime == null) startTime = now;

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
      const palette = readSceneParticlePalette();
      ctx.clearRect(0, 0, W, H);

      const scale = Math.min(W, H) * 0.92;
      const centerX = W / 2;
      const centerY = H / 2;
      const fade = Math.min(1, (now - startTime) / FADE_MS);
      const progress = Math.max(0, Math.min(frames.length - 1, progressRef.current || 0));
      const fromIndex = Math.max(0, Math.min(frames.length - 2, Math.floor(progress)));
      const toIndex = Math.min(frames.length - 1, fromIndex + 1);
      const raw = progress - fromIndex;
      const mix = raw * raw * (3 - 2 * raw);
      const from = frames[fromIndex];
      const to = frames[toIndex];
      const videoMix = video ? smoothstep(frames.length - 2, frames.length - 1, progress) : 0;
      const wantsVideo = videoMix > 0.001 && !reduceMotion;
      const wantsVideoPlayback = progress > frames.length - 2 - VIDEO_PREROLL_PROGRESS && !reduceMotion;

      if (video && wantsVideoPlayback && !isInVideoSegment) {
        isInVideoSegment = true;
        try {
          video.currentTime = VIDEO_ENTRY_OFFSET_S;
        } catch {
          // Browsers may reject a seek before metadata is ready; the frame sampler will still fail closed.
        }
        void video.play().catch(() => {});
      } else if (video && !wantsVideoPlayback && isInVideoSegment) {
        isInVideoSegment = false;
        video.pause();
        try {
          video.currentTime = VIDEO_ENTRY_OFFSET_S;
        } catch {
          // Keep the video loaded; the next segment entry will restart it if seeking is available.
        }
      }

      const dur = video?.duration || 0;
      if (video && wantsVideoPlayback && dur > VIDEO_ENTRY_OFFSET_S + LOOP_RESET_EPSILON_S) {
        const shouldResetLoop =
          video.ended ||
          video.currentTime >= dur - LOOP_RESET_EPSILON_S ||
          video.currentTime < VIDEO_ENTRY_OFFSET_S - LOOP_RESET_EPSILON_S;
        if (shouldResetLoop) {
          try {
            video.currentTime = VIDEO_ENTRY_OFFSET_S;
          } catch {
            // The sampler below will keep using the last readable frame until seeking is available.
          }
          void video.play().catch(() => {});
        }
      }
      if (video && wantsVideo && dur > VIDEO_ENTRY_OFFSET_S + VIDEO_SCROLL_ADVANCE_S) {
        const scrollTime = VIDEO_ENTRY_OFFSET_S + videoMix * VIDEO_SCROLL_ADVANCE_S;
        if (video.currentTime < scrollTime - LOOP_RESET_EPSILON_S) {
          try {
            video.currentTime = scrollTime;
          } catch {
            // Playback pre-roll still supplies a live frame if this seek is not immediately accepted.
          }
        }
      }

      if (video && sampleCtx && sampleW > 0 && sampleH > 0 && wantsVideo) {
        try {
          sampleCtx.drawImage(video, 0, 0, sampleW, sampleH);
          videoFrameData = sampleCtx.getImageData(0, 0, sampleW, sampleH);
        } catch {
          videoFrameData = null;
        }
      } else {
        videoFrameData = null;
      }

      const ct = video?.currentTime ?? 0;
      const seamBlend =
        dur > 0 && !reduceMotion && firstFrameData && videoFrameData && ct > dur - LOOP_BLEND_S
          ? smoothstep(dur - LOOP_BLEND_S, dur, ct)
          : 0;
      const videoTargets = videoFrameData
        ? collectVideoTargets(videoFrameData, firstFrameData, sampleW, sampleH, videoFitX, videoFitY, seamBlend)
        : null;

      for (let i = 0; i < COUNT; i += 1) {
        let targetX = to.x[i];
        let targetY = to.y[i];
        let targetR = to.r[i];
        let targetG = to.g[i];
        let targetB = to.b[i];
        let lightGate = 1;

        if (videoMix > 0 && videoTargets && videoTargets.count > 0) {
          const target = sampleVideoTarget(
            i,
            videoTargets,
            videoSeedU,
          );
          targetX = target.x;
          targetY = target.y;
          targetR += (target.r - targetR) * videoMix;
          targetG += (target.g - targetG) * videoMix;
          targetB += (target.b - targetB) * videoMix;
          lightGate = 1 + (target.gate - 1) * videoMix;
          if (lightGate <= 0.01) continue;
        }

        const x = from.x[i] + (targetX - from.x[i]) * mix;
        const y = from.y[i] + (targetY - from.y[i]) * mix;
        const z = from.z[i] + (to.z[i] - from.z[i]) * mix;
        const r = monochrome ? palette.color[0] : from.r[i] + (targetR - from.r[i]) * mix;
        const g = monochrome ? palette.color[1] : from.g[i] + (targetG - from.g[i]) * mix;
        const b = monochrome ? palette.color[2] : from.b[i] + (targetB - from.b[i]) * mix;
        const weight = from.weight[i] + (to.weight[i] - from.weight[i]) * mix;

        const px = centerX + x * scale;
        const py = centerY + y * scale;
        const lockOpacity = fade * (1 - videoMix);
        if (lockOpacity > 0.001) {
          drawLockDotShader(ctx, {
            x: px,
            y: py,
            depth: z,
            brightness: weight,
            red: r,
            green: g,
            blue: b,
            palette,
            dpr,
            opacity: lockOpacity,
            sizeScale: particleScale,
          });
        }
        if (videoMix > 0.001) {
          const color = monochrome ? { r, g, b } : amplifyColor(r, g, b);
          const [red, green, blue] = sceneParticleColor(palette, color.r, color.g, color.b);
          const size = (0.32 + z * 0.82) * dpr;
          const alpha = Math.min(0.96, (0.34 + z * 0.68) * fade * DIM * lightGate) * videoMix;
          ctx.fillStyle = `rgba(${red},${green},${blue},${alpha})`;
          ctx.beginPath();
          ctx.arc(px, py, size, 0, Math.PI * 2);
          ctx.fill();
        }
      }

      const nextCadence = videoMix > 0.5 ? "active" : "ambient";
      if (pulseCadence !== nextCadence) {
        pulseCadence = nextCadence;
        nextPulseAt = now + dotPulseDelay(nextCadence);
        pulseList = [];
      }
      if (now >= nextPulseAt) {
        spawnPulse(now);
        nextPulseAt = now + dotPulseDelay(nextCadence);
      }
      ctx.save();
      ctx.globalCompositeOperation = palette.darkSurface ? "lighter" : "source-over";
      const nextPulses: typeof pulseList = [];
      for (const pulse of pulseList) {
        const tt = (now - pulse.t0) / pulse.dur;
        if (tt >= 1) continue;
        const i = pulse.idx;
        const x = from.x[i] + (to.x[i] - from.x[i]) * mix;
        const y = from.y[i] + (to.y[i] - from.y[i]) * mix;
        const z = from.z[i] + (to.z[i] - from.z[i]) * mix;
        const px = centerX + x * scale;
        const py = centerY + y * scale;
        const baseR = (0.9 + z * 1.4) * dpr;
        drawDotPulse(ctx, px, py, baseR, tt, fade, palette.pulseInk);
        nextPulses.push(pulse);
      }
      pulseList = nextPulses;
      ctx.restore();

      schedule();
    };

    Promise.all(srcs.map((src, index) => loadFrame(src, frameScales?.[index] ?? 1)))
      .then(async (loaded) => {
        if (stopped) return;
        frames = loaded;
        const last = loaded[loaded.length - 1];
        srcAspect = last.aspect;
        const videoFit = fitUnitBox(srcAspect);
        videoFitX = videoFit.x;
        videoFitY = videoFit.y;
        sampleW = srcAspect >= 1 ? SAMPLE_SIZE : Math.round(SAMPLE_SIZE * srcAspect);
        sampleH = srcAspect >= 1 ? Math.round(SAMPLE_SIZE / srcAspect) : SAMPLE_SIZE;
        sampleCanvas.width = sampleW;
        sampleCanvas.height = sampleH;
        firstFrameData = video
          ? await loadImageData(srcs[srcs.length - 1], sampleW, sampleH).catch(() => null)
          : null;
        pulseIdx = [];
        for (let i = 0; i < COUNT; i += 1) {
          if (loaded.some((frame) => frame.bright[i] === 1)) pulseIdx.push(i);
        }
        schedule();
      })
      .catch((error) => {
        if (stopped) return;
        throw new Error(`Cannot build morph video cloud: ${error instanceof Error ? error.message : String(error)}`);
      });

    const io = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          inViewport = entry.isIntersecting;
          if (!entry.isIntersecting) video?.pause();
          else if (video && isInVideoSegment) void video.play().catch(() => {});
          schedule();
        });
      },
      { threshold: 0.05 },
    );
    io.observe(canvas);

    const onVisibility = () => {
      isVisible = !document.hidden;
      if (!isVisible) {
        video?.pause();
        if (rafId) {
          cancelAnimationFrame(rafId);
          rafId = 0;
        }
      } else if (inViewport) {
        if (video && isInVideoSegment) void video.play().catch(() => {});
      }
      schedule();
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
      io.disconnect();
      video?.pause();
      video?.remove();
      if (video) video.src = "";
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [srcs, videoSrc, progressRef, frameScales, particleScale]);

  return <canvas ref={canvasRef} className={`octopus-cloud ${className}`.trim()} aria-hidden="true" />;
}

function loadFrame(src: string, scale: number): Promise<CloudFrame & { aspect: number }> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(sampleFrame(image, scale));
    image.onerror = () => reject(new Error(`image failed to load: ${src}`));
    image.src = src;
  });
}

function sampleFrame(image: HTMLImageElement, inputScale: number): CloudFrame & { aspect: number } {
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
  const fit = fitUnitBox(aspect);
  const scale = Math.max(0.2, Math.min(1.5, inputScale));
  const x = new Float32Array(COUNT);
  const y = new Float32Array(COUNT);
  const z = new Float32Array(COUNT);
  const r = new Float32Array(COUNT);
  const g = new Float32Array(COUNT);
  const b = new Float32Array(COUNT);
  const weight = new Float32Array(COUNT);
  const bright = new Uint8Array(COUNT);
  let k = 0;
  let attempts = 0;
  const maxAttempts = COUNT * 80;
  while (k < COUNT && attempts < maxAttempts) {
    attempts += 1;
    const fx = Math.random();
    const fy = Math.random();
    const px = Math.min(sw - 1, (fx * sw) | 0);
    const py = Math.min(sh - 1, (fy * sh) | 0);
    const idx = (py * sw + px) * 4;
    const alpha = data[idx + 3] / 255;
    if (alpha < 0.16) continue;
    const rn = data[idx] / 255;
    const gn = data[idx + 1] / 255;
    const bn = data[idx + 2] / 255;
    const lum = rn * 0.2126 + gn * 0.7152 + bn * 0.0722;
    if (lum < LUM_FLOOR) continue;
    if (Math.random() > 0.4 + lum * 0.6) continue;
    x[k] = (fx - 0.5) * 2 * fit.x * scale;
    y[k] = (fy - 0.5) * 2 * fit.y * scale;
    z[k] = Math.max(0.1, Math.min(0.95, 0.45 + (Math.random() - 0.5) * 0.7));
    r[k] = data[idx];
    g[k] = data[idx + 1];
    b[k] = data[idx + 2];
    weight[k] = Math.max(0.28, Math.min(1, alpha * 0.55 + lum * 0.45));
    bright[k] = lum > 0.45 ? 1 : 0;
    k += 1;
  }
  const filled = Math.max(1, k);
  for (; k < COUNT; k += 1) {
    const j = (Math.random() * filled) | 0;
    x[k] = x[j];
    y[k] = y[j];
    z[k] = z[j];
    r[k] = r[j];
    g[k] = g[j];
    b[k] = b[j];
    weight[k] = weight[j];
    bright[k] = bright[j];
  }
  return { x, y, z, r, g, b, weight, bright, aspect };
}

function loadImageData(src: string, width: number, height: number): Promise<ImageData> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d", { willReadFrequently: true });
      if (!ctx) {
        reject(new Error("2d context unavailable"));
        return;
      }
      ctx.drawImage(image, 0, 0, width, height);
      resolve(ctx.getImageData(0, 0, width, height));
    };
    image.onerror = () => reject(new Error(`image failed to load: ${src}`));
    image.src = src;
  });
}

function fitUnitBox(aspect: number) {
  return aspect >= 1 ? { x: 0.5, y: 0.5 / aspect } : { x: 0.5 * aspect, y: 0.5 };
}

function collectVideoTargets(
  frame: ImageData,
  seamFrame: ImageData | null,
  width: number,
  height: number,
  fitX: number,
  fitY: number,
  seamBlend: number,
) {
  const max = Math.ceil((width * height) / 3);
  const x = new Float32Array(max);
  const y = new Float32Array(max);
  const r = new Uint8ClampedArray(max);
  const g = new Uint8ClampedArray(max);
  const b = new Uint8ClampedArray(max);
  const gate = new Float32Array(max);
  let count = 0;

  for (let py = 0; py < height; py += 2) {
    for (let px = 0; px < width; px += 2) {
      const idx = (py * width + px) * 4;
      let rr = frame.data[idx];
      let gg = frame.data[idx + 1];
      let bb = frame.data[idx + 2];
      if (seamFrame && seamBlend > 0) {
        rr += (seamFrame.data[idx] - rr) * seamBlend;
        gg += (seamFrame.data[idx + 1] - gg) * seamBlend;
        bb += (seamFrame.data[idx + 2] - bb) * seamBlend;
      }
      const videoGate = smoothstep(VIDEO_LUM_FLOOR, VIDEO_LUM_FULL, luminance(rr, gg, bb));
      if (videoGate <= 0.05) continue;
      if (count >= max) break;
      x[count] = (px / Math.max(1, width - 1) - 0.5) * 2 * fitX;
      y[count] = (py / Math.max(1, height - 1) - 0.5) * 2 * fitY;
      r[count] = rr;
      g[count] = gg;
      b[count] = bb;
      gate[count] = videoGate;
      count += 1;
    }
  }

  return { count, x, y, r, g, b, gate };
}

function sampleVideoTarget(pointIndex: number, targets: VideoTargets, seedU: Float32Array) {
  let index = Math.floor(seedU[pointIndex] * targets.count);
  if (index >= targets.count) index = targets.count - 1;
  if (index < 0) index = 0;

  return {
    x: targets.x[index],
    y: targets.y[index],
    r: targets.r[index],
    g: targets.g[index],
    b: targets.b[index],
    gate: targets.gate[index],
  };
}

function hash01(index: number, salt: number) {
  let x = (index + 1) * 374761393 + salt * 668265263;
  x = (x ^ (x >>> 13)) * 1274126177;
  return ((x ^ (x >>> 16)) >>> 0) / 4294967296;
}

function fract(value: number) {
  return value - Math.floor(value);
}

function smoothstep(edge0: number, edge1: number, x: number) {
  const t = Math.max(0, Math.min(1, (x - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
}

function luminance(r: number, g: number, b: number): number {
  return r * 0.2126 + g * 0.7152 + b * 0.0722;
}

function amplifyColor(r: number, g: number, b: number) {
  const avg = (r + g + b) / 3;
  const saturation = 1.5;
  const contrast = 1.14;
  const lift = 10;
  return {
    r: clampColor((avg + (r - avg) * saturation - 128) * contrast + 128 + lift),
    g: clampColor((avg + (g - avg) * saturation - 128) * contrast + 128 + lift),
    b: clampColor((avg + (b - avg) * saturation - 128) * contrast + 128 + lift),
  };
}

function clampColor(value: number) {
  return Math.max(0, Math.min(255, Math.round(value)));
}
