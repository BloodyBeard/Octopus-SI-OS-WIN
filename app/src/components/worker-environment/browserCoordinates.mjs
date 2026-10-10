export function browserImagePoint(bounds, deviceWidth, clientX, clientY, clamp = false) {
  if (!bounds || !Number.isFinite(deviceWidth) || deviceWidth <= 0 || bounds.width <= 0 || bounds.height <= 0) return null;
  const localX = clientX - bounds.left;
  const localY = clientY - bounds.top;
  if (!Number.isFinite(localX) || !Number.isFinite(localY)) return null;
  if (!clamp && (localX < 0 || localY < 0 || localX > bounds.width || localY > bounds.height)) return null;
  const zoom = bounds.width / deviceWidth;
  return {
    x: Math.round(Math.max(0, Math.min(bounds.width, localX)) / zoom),
    y: Math.round(Math.max(0, Math.min(bounds.height, localY)) / zoom),
    zoom,
  };
}
