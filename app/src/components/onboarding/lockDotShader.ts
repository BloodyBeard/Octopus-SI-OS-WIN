import { sceneParticleColor, type SceneParticlePalette } from "../../design-system/sceneParticlePalette";

type LockDotShaderInput = {
  x: number;
  y: number;
  depth: number;
  brightness: number;
  red: number;
  green: number;
  blue: number;
  palette: SceneParticlePalette;
  dpr: number;
  opacity?: number;
  sizeScale?: number;
};

export function drawLockDotShader(
  context: CanvasRenderingContext2D,
  input: LockDotShaderInput,
) {
  const opacity = input.opacity ?? 1;
  const size = (0.14 + input.depth * 0.38) * (0.55 + input.brightness * 0.45) * input.dpr * (input.sizeScale ?? 1);
  const alpha = (0.26 + input.depth * 0.74) * input.brightness * opacity * 0.8;
  const [red, green, blue] = sceneParticleColor(input.palette, input.red, input.green, input.blue);
  context.fillStyle = `rgba(${red},${green},${blue},${alpha})`;
  context.fillRect(input.x - size, input.y - size, size * 2, size * 2);
}
