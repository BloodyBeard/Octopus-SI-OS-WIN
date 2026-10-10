export function browserImagePoint(
  bounds: { width: number; height: number; left: number; top: number } | null | undefined,
  deviceWidth: number | undefined,
  clientX: number,
  clientY: number,
  clamp?: boolean,
): { x: number; y: number; zoom: number } | null;
