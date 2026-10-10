export function roundedRectanglePath(
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): string {
  const k = (radius * 4 * (Math.sqrt(2) - 1)) / 3;
  const n = (value: number) => String(Number(value.toPrecision(6)));
  return (
    `M${n(x)} ${n(y + radius)}` +
    `C${n(x)} ${n(y + radius - k)} ${n(x + radius - k)} ${n(y)} ${n(x + radius)} ${n(y)}` +
    `H${n(x + width - radius)}` +
    `C${n(x + width - radius + k)} ${n(y)} ${n(x + width)} ${n(y + radius - k)} ${n(x + width)} ${n(y + radius)}` +
    `V${n(y + height - radius)}` +
    `C${n(x + width)} ${n(y + height - radius + k)} ${n(x + width - radius + k)} ${n(y + height)} ${n(x + width - radius)} ${n(y + height)}` +
    `H${n(x + radius)}` +
    `C${n(x + radius - k)} ${n(y + height)} ${n(x)} ${n(y + height - radius + k)} ${n(x)} ${n(y + height - radius)}` +
    `V${n(y + radius)}Z`
  );
}

const n = (v: number) => String(Number(v.toPrecision(6))),
  p = (x: number, y: number) => `${n(x)} ${n(y)}`;
function cap(x: number, y: number, r = 0.66504, half = false) {
  const scale = r / 0.66504;
  const upper = `L${p(x + 0.13379 * scale, y - 0.65137 * scale)}C${p(x + 0.43692 * scale, y - 0.58943 * scale)} ${p(x + r, y - 0.32143 * scale)} ${p(x + r, y)}`;
  return half
    ? upper
    : upper +
        `C${p(x + r, y + 0.32143 * scale)} ${p(x + 0.43692 * scale, y + 0.58943 * scale)} ${p(x + 0.13379 * scale, y + 0.65137 * scale)}L${p(x, y + r)}`;
}
export function roundedCrossPath(
  cx: number,
  cy: number,
  a: number,
  r = 0.66504,
  t = 0.36727,
) {
  const l = cx - a,
    rr = cx + a,
    top = cy - a,
    b = cy + a;
  return `M${p(cx - r, b)}V${n(cy + r)}H${n(l)}C${p(l - t, cy + r)} ${p(l - r, cy + t)} ${p(l - r, cy)}C${p(l - r, cy - t)} ${p(l - t, cy - r)} ${p(l, cy - r)}H${n(cx - r)}V${n(top)}C${p(cx - r, top - t)} ${p(cx - t, top - r)} ${p(cx, top - r)}C${p(cx + t, top - r)} ${p(cx + r, top - t)} ${p(cx + r, top)}V${n(cy - r)}H${n(rr)}${cap(rr, cy, r)}H${n(cx + r)}V${n(b)}C${p(cx + r, b + t)} ${p(cx + t, b + r)} ${p(cx, b + r)}C${p(cx - t, b + r)} ${p(cx - r, b + t)} ${p(cx - r, b)}Z`;
}
export function cubicCirclePath(
  cx: number,
  cy: number,
  r: number,
  cxControl: number,
  cyControl = cxControl,
  reverse = false,
) {
  const t = cy - r,
    b = cy + r,
    l = cx - r,
    rr = cx + r,
    c = cxControl,
    v = cyControl;
  return reverse
    ? `M${p(cx, t)}C${p(cx - c, t)} ${p(l, cy - v)} ${p(l, cy)}C${p(l, cy + v)} ${p(cx - c, b)} ${p(cx, b)}C${p(cx + c, b)} ${p(rr, cy + v)} ${p(rr, cy)}C${p(rr, cy - v)} ${p(cx + c, t)} ${p(cx, t)}Z`
    : `M${p(cx, t)}C${p(cx + c, t)} ${p(rr, cy - v)} ${p(rr, cy)}C${p(rr, cy + v)} ${p(cx + c, b)} ${p(cx, b)}C${p(cx - c, b)} ${p(l, cy + v)} ${p(l, cy)}C${p(l, cy - v)} ${p(cx - c, t)} ${p(cx, t)}Z`;
}
export function threeQuarterRingPath(x: number, y: number, outer: number, inner: number): string {
  const k = 4 * (Math.sqrt(2) - 1) / 3;
  return `M${p(x, y - outer)}`
    + `C${p(x + outer * k, y - outer)} ${p(x + outer, y - outer * k)} ${p(x + outer, y)}`
    + `C${p(x + outer, y + outer * k)} ${p(x + outer * k, y + outer)} ${p(x, y + outer)}`
    + `C${p(x - outer * k, y + outer)} ${p(x - outer, y + outer * k)} ${p(x - outer, y)}`
    + `H${n(x - inner)}`
    + `C${p(x - inner, y + inner * k)} ${p(x - inner * k, y + inner)} ${p(x, y + inner)}`
    + `C${p(x + inner * k, y + inner)} ${p(x + inner, y + inner * k)} ${p(x + inner, y)}`
    + `C${p(x + inner, y - inner * k)} ${p(x + inner * k, y - inner)} ${p(x, y - inner)}Z`;
}

export function expandCornersPath(
  lo: number,
  hi: number,
  arm: number,
  r = 0.66504,
  t = 0.36727,
) {
  const tip = hi - arm,
    edge = lo + arm,
    upper = hi - arm;
  return (
    `M${p(lo - r, tip)}C${p(lo - r, tip - t)} ${p(lo - t, tip - r)} ${p(lo, tip - r)}C${p(lo + t, tip - r)} ${p(lo + r, tip - t)} ${p(lo + r, tip)}V${n(hi - r)}H${n(edge)}${cap(edge, hi, r)}H${n(lo)}C${p(lo - t, hi + r)} ${p(lo - r, hi + t)} ${p(lo - r, hi)}V${n(tip)}Z` +
    `M${p(hi - r, edge)}V${n(lo + r)}H${n(upper)}C${p(upper - t, lo + r)} ${p(upper - r, lo + t)} ${p(upper - r, lo)}C${p(upper - r, lo - t)} ${p(upper - t, lo - r)} ${p(upper, lo - r)}H${n(hi)}${cap(hi, lo, r, true)}V${n(edge)}C${p(hi + r, edge + t)} ${p(hi + t, edge + r)} ${p(hi, edge + r)}C${p(hi - t, edge + r)} ${p(hi - r, edge + t)} ${p(hi - r, edge)}Z`
  );
}

export function uniformRoundedCrossPath(
  cx: number,
  cy: number,
  a: number,
  r: number,
  t: number,
) {
  const top = cy - a,
    b = cy + a,
    l = cx - a,
    rr = cx + a;
  return `M${p(cx, top - r)}C${p(cx + t, top - r)} ${p(cx + r, top - t)} ${p(cx + r, top)}V${n(cy - r)}H${n(rr)}C${p(rr + t, cy - r)} ${p(rr + r, cy - t)} ${p(rr + r, cy)}C${p(rr + r, cy + t)} ${p(rr + t, cy + r)} ${p(rr, cy + r)}H${n(cx + r)}V${n(b)}C${p(cx + r, b + t)} ${p(cx + t, b + r)} ${p(cx, b + r)}C${p(cx - t, b + r)} ${p(cx - r, b + t)} ${p(cx - r, b)}V${n(cy + r)}H${n(l)}C${p(l - t, cy + r)} ${p(l - r, cy + t)} ${p(l - r, cy)}C${p(l - r, cy - t)} ${p(l - t, cy - r)} ${p(l, cy - r)}H${n(cx - r)}V${n(top)}C${p(cx - r, top - t)} ${p(cx - t, top - r)} ${p(cx, top - r)}Z`;
}
export function cubicEllipsePath(
  cx: number,
  cy: number,
  rx: number,
  ry: number,
  h: number,
  v = h,
  reverse = false,
) {
  const t = cy - ry,
    b = cy + ry,
    l = cx - rx,
    r = cx + rx;
  return reverse
    ? `M${p(cx, t)}C${p(cx - h, t)} ${p(l, cy - v)} ${p(l, cy)}C${p(l, cy + v)} ${p(cx - h, b)} ${p(cx, b)}C${p(cx + h, b)} ${p(r, cy + v)} ${p(r, cy)}C${p(r, cy - v)} ${p(cx + h, t)} ${p(cx, t)}Z`
    : `M${p(cx, t)}C${p(cx + h, t)} ${p(r, cy - v)} ${p(r, cy)}C${p(r, cy + v)} ${p(cx + h, b)} ${p(cx, b)}C${p(cx - h, b)} ${p(l, cy + v)} ${p(l, cy)}C${p(l, cy - v)} ${p(cx - h, t)} ${p(cx, t)}Z`;
}
export function orthogonalBranchPath(
  left: number,
  startY: number,
  cornerX: number,
  cornerY: number,
  midY: number,
  endX: number,
  right: number,
  endY: number,
  lastY: number,
  h1: number,
  v1: number,
  h2: number,
  v2: number,
) {
  return `M${p(left, startY)}V${n(cornerY)}C${p(left, cornerY - v1)} ${p(cornerX - h1, midY)} ${p(cornerX, midY)}H${n(endX)}C${p(endX + h2, midY)} ${p(right, endY + v2)} ${p(right, endY)}V${n(lastY)}`;
}
export function circularArcRingPath(
  cx: number,
  cy: number,
  outer: number,
  inner: number,
) {
  return `M${p(cx, cy - outer)}a${n(outer)} ${n(outer)} 0 1 1 0 ${n(2 * outer)} ${n(outer)} ${n(outer)} 0 0 1 0 ${n(-2 * outer)}Zm0 ${n(outer - inner)}a${n(inner)} ${n(inner)} 0 1 0 0 ${n(2 * inner)} ${n(inner)} ${n(inner)} 0 0 0 0 ${n(-2 * inner)}Z`;
}
