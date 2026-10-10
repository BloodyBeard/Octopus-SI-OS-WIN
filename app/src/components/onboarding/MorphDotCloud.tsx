import { useEffect, useRef } from "react";

// A dot cloud that MORPHS between several icons as a scroll container is paged.
// Every icon is sampled to the SAME number of dots, so dot k flows from its
// position in icon i to its position in icon i+1 as the scroll crosses that
// section — an organic particle morph. Flat 2D (no 3D tilt), with the same
// twinkle pulses as the rest of the onboarding.

type Props = {
  srcs: string[];
  /** Current scroll progress in icon units (0 .. srcs.length - 1). */
  progressRef: React.RefObject<number>;
  className?: string;
};

const COUNT = 4500;
const SAMPLE_SIZE = 300;
const LUM_FLOOR = 0.1;
const FADE_MS = 700;

type IconDots = {
  x: Float32Array;
  y: Float32Array;
  z: Float32Array;
  r: Float32Array;
  g: Float32Array;
  b: Float32Array;
  colored: Uint8Array;
};

export function MorphDotCloud({ srcs, progressRef, className = "" }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let stopped = false;
    let rafId = 0;
    let icons: IconDots[] | null = null;
    let coloredIdx: number[] = [];
    let brightIdx: number[] = [];
    let pulseList: Array<{ idx: number; t0: number; dur: number }> = [];
    let lastSpawn = 0;
    let startTime: number | null = null;

    const canvas = canvasRef.current;
    if (!canvas) return;

    const spawnPulse = (now: number) => {
      const pool = coloredIdx.length > 0 && Math.random() < 0.5 ? coloredIdx : brightIdx;
      if (pool.length === 0) return;
      pulseList.push({ idx: pool[(Math.random() * pool.length) | 0], t0: now, dur: 1200 + Math.random() * 1040 });
    };

    const draw = (now: number) => {
      rafId = 0;
      if (stopped || !icons) return;
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
      ctx.clearRect(0, 0, W, H);

      const S = Math.min(W, H) * 0.92;
      const cxp = W / 2;
      const cyp = H / 2;
      const fade = Math.min(1, (now - startTime) / FADE_MS);

      // Scroll progress (in icon units) → which icons to blend.
      let p = progressRef.current || 0;
      const n = icons.length;
      p = Math.max(0, Math.min(n - 1, p));
      const i = Math.max(0, Math.min(n - 2, Math.floor(p)));
      const raw = p - i;
      const f = raw * raw * (3 - 2 * raw); // smoothstep
      const a = icons[i];
      const b = icons[i + 1];

      for (let k = 0; k < COUNT; k += 1) {
        const cx = a.x[k] + (b.x[k] - a.x[k]) * f;
        const cy = a.y[k] + (b.y[k] - a.y[k]) * f;
        const z = a.z[k] + (b.z[k] - a.z[k]) * f;
        const px = cxp + cx * S;
        const py = cyp + cy * S;
        const r = Math.round(a.r[k] + (b.r[k] - a.r[k]) * f);
        const g = Math.round(a.g[k] + (b.g[k] - a.g[k]) * f);
        const bl = Math.round(a.b[k] + (b.b[k] - a.b[k]) * f);
        const size = (0.34 + z * 0.55) * dpr;
        const alpha = (0.22 + z * 0.5) * fade;
        ctx.fillStyle = `rgba(${r},${g},${bl},${alpha})`;
        ctx.fillRect(px - size, py - size, size * 2, size * 2);
      }

      // Twinkle (computed against the blended positions).
      if (now - lastSpawn > 180 + Math.random() * 220) {
        spawnPulse(now);
        lastSpawn = now;
      }
      ctx.save();
      ctx.globalCompositeOperation = "lighter";
      const glow = getGlowSprite();
      const next: typeof pulseList = [];
      for (const pulse of pulseList) {
        const tt = (now - pulse.t0) / pulse.dur;
        if (tt >= 1) continue;
        const k = pulse.idx;
        const cx = a.x[k] + (b.x[k] - a.x[k]) * f;
        const cy = a.y[k] + (b.y[k] - a.y[k]) * f;
        const px = cxp + cx * S;
        const py = cyp + cy * S;
        const rise = Math.min(1, tt * 6);
        const decay = Math.pow(1 - tt, 1.8);
        const intensity = rise * decay * fade;
        const baseR = 1.1 * dpr;
        const cr = baseR * 1.2;
        ctx.globalAlpha = intensity;
        ctx.fillStyle = "#ffffff";
        ctx.fillRect(px - cr, py - cr, cr * 2, cr * 2);
        const haloR = baseR * (4 + tt * 10);
        ctx.globalAlpha = intensity * 0.7;
        ctx.drawImage(glow, px - haloR, py - haloR, haloR * 2, haloR * 2);
        ctx.globalAlpha = 1;
        next.push(pulse);
      }
      pulseList = next;
      ctx.restore();

      schedule();
    };

    const schedule = () => {
      if (stopped || rafId) return;
      rafId = requestAnimationFrame(draw);
    };

    Promise.all(srcs.map((src) => loadIcon(src)))
      .then((result) => {
        if (stopped) return;
        icons = result;
        coloredIdx = [];
        brightIdx = [];
        for (let k = 0; k < COUNT; k += 1) {
          if (result.some((ic) => ic.colored[k] === 1)) coloredIdx.push(k);
          else if (result[0].z[k] > 0.5) brightIdx.push(k);
        }
        schedule();
      })
      .catch((error) => {
        if (stopped) return;
        throw new Error(`Cannot build morph cloud: ${error instanceof Error ? error.message : String(error)}`);
      });

    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
    };
  }, [srcs, progressRef]);

  return <canvas ref={canvasRef} className={`octopus-cloud ${className}`.trim()} aria-hidden="true" />;
}

let glowSprite: HTMLCanvasElement | null = null;

function getGlowSprite(): HTMLCanvasElement {
  if (glowSprite) return glowSprite;
  const c = document.createElement("canvas");
  c.width = 64;
  c.height = 64;
  const g = c.getContext("2d");
  if (g) {
    const grad = g.createRadialGradient(32, 32, 0, 32, 32, 32);
    grad.addColorStop(0, "rgba(255,255,255,1)");
    grad.addColorStop(0.4, "rgba(255,255,255,0.35)");
    grad.addColorStop(1, "rgba(255,255,255,0)");
    g.fillStyle = grad;
    g.fillRect(0, 0, 64, 64);
  }
  glowSprite = c;
  return c;
}

function loadIcon(src: string): Promise<IconDots> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(sampleIcon(image));
    image.onerror = () => reject(new Error(`image failed to load: ${src}`));
    image.src = src;
  });
}

function sampleIcon(image: HTMLImageElement): IconDots {
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

  // Aspect fit into a centered unit box so every icon shares one coord space.
  const fitX = aspect >= 1 ? 0.5 : 0.5 * aspect;
  const fitY = aspect >= 1 ? 0.5 / aspect : 0.5;

  const x = new Float32Array(COUNT);
  const y = new Float32Array(COUNT);
  const z = new Float32Array(COUNT);
  const r = new Float32Array(COUNT);
  const g = new Float32Array(COUNT);
  const b = new Float32Array(COUNT);
  const colored = new Uint8Array(COUNT);

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
    const al = data[idx + 3] / 255;
    if (al < 0.16) continue;
    const rn = data[idx] / 255;
    const gn = data[idx + 1] / 255;
    const bn = data[idx + 2] / 255;
    const lum = rn * 0.2126 + gn * 0.7152 + bn * 0.0722;
    if (lum < LUM_FLOOR) continue;
    if (Math.random() > 0.4 + lum * 0.6) continue;
    const max = Math.max(rn, gn, bn);
    const min = Math.min(rn, gn, bn);
    const sat = max <= 0.001 ? 0 : (max - min) / max;
    const isColored = sat > 0.26 && max > 0.32;
    x[k] = (fx - 0.5) * 2 * fitX;
    y[k] = (fy - 0.5) * 2 * fitY;
    z[k] = Math.max(0.1, Math.min(0.95, 0.45 + (Math.random() - 0.5) * 0.7));
    r[k] = isColored ? Math.min(255, data[idx] + 24) : 236;
    g[k] = isColored ? Math.min(255, data[idx + 1] + 24) : 238;
    b[k] = isColored ? Math.min(255, data[idx + 2] + 24) : 244;
    colored[k] = isColored ? 1 : 0;
    k += 1;
  }
  // Pad if the icon was sparse, so every icon has exactly COUNT dots.
  const filled = Math.max(1, k);
  for (; k < COUNT; k += 1) {
    const j = (Math.random() * filled) | 0;
    x[k] = x[j];
    y[k] = y[j];
    z[k] = z[j];
    r[k] = r[j];
    g[k] = g[j];
    b[k] = b[j];
    colored[k] = colored[j];
  }

  return { x, y, z, r, g, b, colored };
}
