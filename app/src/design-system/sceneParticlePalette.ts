type ParticleColor = readonly [number, number, number];

export const LIGHT_SCENE_PARTICLE_COLOR: ParticleColor = [0.30, 0.62, 0.96];
export const DARK_SCENE_PARTICLE_COLOR: ParticleColor = [235, 238, 244];

export type SceneParticlePalette = {
  darkSurface: boolean;
  color: ParticleColor;
  pulseInk: string;
};

const lightColor: ParticleColor = [
  LIGHT_SCENE_PARTICLE_COLOR[0] * 255,
  LIGHT_SCENE_PARTICLE_COLOR[1] * 255,
  LIGHT_SCENE_PARTICLE_COLOR[2] * 255,
];
const lightPalette: SceneParticlePalette = { darkSurface: false, color: lightColor, pulseInk: lightColor.join(",") };
const darkPalette: SceneParticlePalette = { darkSurface: true, color: DARK_SCENE_PARTICLE_COLOR, pulseInk: "255,255,255" };

export function readSceneParticlePalette(): SceneParticlePalette {
  return document.documentElement.dataset.theme === "light" ? lightPalette : darkPalette;
}

export function sceneParticleColor(palette: SceneParticlePalette, red: number, green: number, blue: number): ParticleColor {
  if (!palette.darkSurface) {
    const max = Math.max(red, green, blue);
    const min = Math.min(red, green, blue);
    const colored = max > 0.32 * 255 && max - min > max * 0.26;
    if (!colored) return palette.color;
  }
  return [red, green, blue];
}
