import { useEffect, useRef } from "react";

type DotVariant = "welcome" | "kernel";

type Props = {
  variant: DotVariant;
};

type Dot = [number, number, number, number];

const LOGO_SRC = "./octopus-logo.png";
const SAMPLE_SIZE = 212;
const SAMPLE_STEP = 3;
const MAX_DOTS = 3000;

export function DottedLogoCanvas({ variant }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const mouseRef = useRef({ x: 0, y: 0, active: false });

  useEffect(() => {
    let stopped = false;
    let rafId = 0;
    let isVisible = true;
    let inViewport = false;
    let dots: Dot[] | null = null;
    let brightIdx: number[] | null = null;
    let pulseList: Array<{ idx: number; t0: number; dur: number }> = [];
    let lastSpawn = 0;
    let srcW = 1;
    let srcH = 1;
    let mx = 0;
    let my = 0;
    let revealStart: number | null = null;

    const mode = variant === "kernel" ? "flat" : "3d";
    const mouseWeight = variant === "kernel" ? 0.28 : 0.42;
    const idleScale = variant === "kernel" ? 0.55 : 0.8;
    const pulses = variant === "welcome";
    const sizeBase = 0.22;
    const sizeBrightMul = 0.65;
    const alphaBase = 0.22;
    const alphaBrightMul = 0.72;
    const revealSweepMs = 700;
    const revealFadeMs = 900;

    const spawnPulse = (now: number) => {
      if (!brightIdx || brightIdx.length === 0) return;
      const idx = brightIdx[(Math.random() * brightIdx.length) | 0];
      const dur = 600 + Math.random() * 500;
      pulseList.push({ idx, t0: now, dur });
    };

    const schedule = () => {
      if (stopped || rafId || !isVisible || !inViewport) return;
      rafId = requestAnimationFrame(draw);
    };

    const draw = (now: number) => {
      rafId = 0;
      if (stopped) return;
      const canvas = canvasRef.current;
      if (!canvas || !dots || !isVisible || !inViewport) return;

      const rect = canvas.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const cssW = Math.max(1, rect.width);
      const cssH = Math.max(1, rect.height);
      const width = Math.floor(cssW * dpr);
      const height = Math.floor(cssH * dpr);
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }

      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, width, height);

      const srcAspect = srcW / srcH;
      const dstAspect = cssW / cssH;
      let drawW: number;
      let drawH: number;
      let offX: number;
      let offY: number;
      if (srcAspect > dstAspect) {
        drawW = width;
        drawH = width / srcAspect;
        offX = 0;
        offY = (height - drawH) / 2;
      } else {
        drawH = height;
        drawW = height * srcAspect;
        offY = 0;
        offX = (width - drawW) / 2;
      }

      if (mode === "flat") {
        const pad = 28 * dpr;
        const scaleW = Math.max(0.6, 1 - (pad * 2) / Math.max(drawW, 1));
        const scaleH = Math.max(0.6, 1 - (pad * 2) / Math.max(drawH, 1));
        const scale = Math.min(scaleW, scaleH);
        const nextW = drawW * scale;
        const nextH = drawH * scale;
        offX += (drawW - nextW) / 2;
        offY += (drawH - nextH) / 2;
        drawW = nextW;
        drawH = nextH;
      }

      const mouseTargetX = mouseRef.current.active ? mouseRef.current.x : 0;
      const mouseTargetY = mouseRef.current.active ? mouseRef.current.y : 0;
      mx += (mouseTargetX - mx) * 0.08;
      my += (mouseTargetY - my) * 0.08;

      const t = now * 0.001;
      const elapsed = revealStart == null ? -1 : now - revealStart;

      if (mode === "flat") {
        const panX = (Math.sin(t * 0.22) * 3 + mx * 8 * mouseWeight) * dpr;
        const panY = (Math.sin(t * 0.15) * 2.5 + my * 6 * mouseWeight) * dpr;
        for (let i = 0; i < dots.length; i += 1) {
          const dot = dots[i];
          const revealA = revealAlpha(i, elapsed, revealSweepMs, revealFadeMs);
          if (revealA <= 0) continue;
          const brightness = dot[3] ?? 1;
          const px = offX + dot[0] * drawW + panX;
          const py = offY + dot[1] * drawH + panY;
          const size = 0.7 * (0.78 + brightness * 0.22) * dpr;
          const alpha = 0.58 * (0.4 + brightness * 0.6) * revealA;
          ctx.fillStyle = `rgba(255,255,255,${alpha})`;
          ctx.beginPath();
          ctx.arc(px, py, size, 0, Math.PI * 2);
          ctx.fill();
        }
      } else {
        const angY = Math.sin(t * 0.22) * 0.05 * idleScale + mx * 0.22 * mouseWeight;
        const angX = Math.sin(t * 0.15) * 0.03 * idleScale + my * -0.16 * mouseWeight;
        const cosX = Math.cos(angX);
        const sinX = Math.sin(angX);
        const cosY = Math.cos(angY);
        const sinY = Math.sin(angY);
        const cxN = 0.5;
        const cyN = 0.5;
        const depthScale = 0.42;

        for (let i = 0; i < dots.length; i += 1) {
          const dot = dots[i];
          const revealA = revealAlpha(i, elapsed, revealSweepMs, revealFadeMs);
          if (revealA <= 0) continue;
          const nx = dot[0] - cxN;
          const ny = dot[1] - cyN;
          const nz = dot[2] * depthScale;
          const brightness = dot[3] ?? 1;
          const x1 = nx * cosY + nz * sinY;
          const z1 = -nx * sinY + nz * cosY;
          const y1 = ny * cosX - z1 * sinX;
          const z2 = ny * sinX + z1 * cosX;
          const px = offX + (cxN + x1) * drawW;
          const py = offY + (cyN + y1) * drawH;
          const depthT = 0.5 + (z2 / depthScale) * 0.5;
          const zVis = Math.max(0, Math.min(1, depthT));
          const size = (sizeBase + zVis * sizeBrightMul) * (0.55 + brightness * 0.45) * dpr;
          const alpha = (alphaBase + zVis * alphaBrightMul) * brightness * revealA;
          ctx.fillStyle = `rgba(255,255,255,${alpha})`;
          ctx.beginPath();
          ctx.arc(px, py, size, 0, Math.PI * 2);
          ctx.fill();
        }

        if (pulses) {
          const revealDone = elapsed > revealSweepMs + revealFadeMs;
          if (revealDone && now - lastSpawn > 110 + Math.random() * 170) {
            spawnPulse(now);
            if (Math.random() < 0.35) spawnPulse(now);
            lastSpawn = now;
          }
          ctx.save();
          ctx.globalCompositeOperation = "lighter";
          const next: typeof pulseList = [];
          for (const pulse of pulseList) {
            const progress = (now - pulse.t0) / pulse.dur;
            if (progress >= 1) continue;
            const dot = dots[pulse.idx];
            if (!dot) continue;
            const nx = dot[0] - cxN;
            const ny = dot[1] - cyN;
            const nz = dot[2] * depthScale;
            const x1 = nx * cosY + nz * sinY;
            const z1 = -nx * sinY + nz * cosY;
            const y1 = ny * cosX - z1 * sinX;
            const px = offX + (cxN + x1) * drawW;
            const py = offY + (cyN + y1) * drawH;
            const rise = Math.min(1, progress * 6);
            const decay = Math.pow(1 - progress, 1.8);
            const intensity = rise * decay;
            const radius = (0.9 + dot[2] * 1.4) * dpr;
            ctx.fillStyle = `rgba(255,255,255,${intensity})`;
            ctx.beginPath();
            ctx.arc(px, py, radius * 1.2, 0, Math.PI * 2);
            ctx.fill();
            const haloR = radius * (4 + progress * 10);
            const grad = ctx.createRadialGradient(px, py, 0, px, py, haloR);
            grad.addColorStop(0, `rgba(210,225,255,${0.85 * intensity})`);
            grad.addColorStop(0.4, `rgba(210,225,255,${0.25 * intensity})`);
            grad.addColorStop(1, "rgba(210,225,255,0)");
            ctx.fillStyle = grad;
            ctx.beginPath();
            ctx.arc(px, py, haloR, 0, Math.PI * 2);
            ctx.fill();
            next.push(pulse);
          }
          pulseList = next;
          ctx.restore();
        }
      }

      schedule();
    };

    loadLogoDots().then((nextDots) => {
      if (stopped) return;
      dots = nextDots;
      srcW = 1;
      srcH = 1;
      brightIdx = [];
      for (let i = 0; i < dots.length; i += 1) {
        const dot = dots[i];
        if (dot[3] > 0.5 && dot[2] > 0.35) brightIdx.push(i);
      }
      schedule();
    }).catch((error) => {
      if (stopped) return;
      throw new Error(`Cannot load Octopus dot mark: ${error instanceof Error ? error.message : String(error)}`);
    });

    const canvas = canvasRef.current;
    let intersectionObserver: IntersectionObserver | null = null;
    if (canvas) {
      intersectionObserver = new IntersectionObserver((entries) => {
        entries.forEach((entry) => {
          inViewport = entry.isIntersecting;
          if (entry.isIntersecting && revealStart == null) {
            revealStart = performance.now();
          }
          schedule();
        });
      }, { threshold: 0.1 });
      intersectionObserver.observe(canvas);
    }

    const onVisibility = () => {
      isVisible = !document.hidden;
      if (!isVisible && rafId) {
        cancelAnimationFrame(rafId);
        rafId = 0;
      }
      schedule();
    };

    const onMove = (event: MouseEvent) => {
      const current = canvasRef.current;
      if (!current) return;
      const rect = current.getBoundingClientRect();
      const centerX = rect.left + rect.width / 2;
      const centerY = rect.top + rect.height / 2;
      mouseRef.current.x = Math.max(-1, Math.min(1, (event.clientX - centerX) / (rect.width * 0.5)));
      mouseRef.current.y = Math.max(-1, Math.min(1, (event.clientY - centerY) / (rect.height * 0.5)));
      mouseRef.current.active = true;
    };
    const onLeave = () => {
      mouseRef.current.active = false;
    };
    const allowMouse = window.matchMedia("(hover: hover) and (pointer: fine)").matches;
    if (allowMouse) {
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseleave", onLeave);
    }
    document.addEventListener("visibilitychange", onVisibility);
    onVisibility();

    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
      intersectionObserver?.disconnect();
      if (allowMouse) {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseleave", onLeave);
      }
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [variant]);

  return <canvas ref={canvasRef} className="welcome-dot-canvas" aria-hidden="true" />;
}

function loadLogoDots(): Promise<Dot[]> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(sampleLogo(image));
    image.onerror = () => reject(new Error(`image failed to load: ${LOGO_SRC}`));
    image.src = LOGO_SRC;
  });
}

function sampleLogo(image: HTMLImageElement): Dot[] {
  const canvas = document.createElement("canvas");
  canvas.width = SAMPLE_SIZE;
  canvas.height = SAMPLE_SIZE;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) return [];

  const ratio = Math.min(SAMPLE_SIZE / image.naturalWidth, SAMPLE_SIZE / image.naturalHeight) * 0.9;
  const width = image.naturalWidth * ratio;
  const height = image.naturalHeight * ratio;
  const offX = (SAMPLE_SIZE - width) / 2;
  const offY = (SAMPLE_SIZE - height) / 2;
  ctx.clearRect(0, 0, SAMPLE_SIZE, SAMPLE_SIZE);
  ctx.drawImage(image, offX, offY, width, height);

  const pixels = ctx.getImageData(0, 0, SAMPLE_SIZE, SAMPLE_SIZE).data;
  const candidates: Dot[] = [];
  for (let y = 0; y < SAMPLE_SIZE; y += SAMPLE_STEP) {
    for (let x = 0; x < SAMPLE_SIZE; x += SAMPLE_STEP) {
      const index = (y * SAMPLE_SIZE + x) * 4;
      const alpha = pixels[index + 3] / 255;
      if (alpha < 0.16) continue;
      const red = pixels[index] / 255;
      const green = pixels[index + 1] / 255;
      const blue = pixels[index + 2] / 255;
      const luminance = red * 0.2126 + green * 0.7152 + blue * 0.0722;
      const nx = x / SAMPLE_SIZE - 0.5;
      const ny = y / SAMPLE_SIZE - 0.5;
      const distance = Math.sqrt(nx * nx + ny * ny);
      const depth = Math.max(0, Math.min(1, alpha * 0.72 + (0.52 - distance) * 0.42));
      const brightness = Math.max(0.2, Math.min(1, alpha * 0.72 + luminance * 0.28));
      candidates.push([x / SAMPLE_SIZE, y / SAMPLE_SIZE, depth, brightness]);
    }
  }

  if (candidates.length <= MAX_DOTS) return candidates;
  const dots: Dot[] = [];
  const stride = candidates.length / MAX_DOTS;
  for (let i = 0; i < MAX_DOTS; i += 1) {
    dots.push(candidates[Math.floor(i * stride)]);
  }
  return dots;
}

function revealAlpha(index: number, elapsed: number, sweepMs: number, fadeMs: number) {
  if (elapsed < 0) return 0;
  const hash = (index * 0.61803398875) % 1;
  const t = (elapsed - hash * sweepMs) / fadeMs;
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  return t * t * (3 - 2 * t);
}
