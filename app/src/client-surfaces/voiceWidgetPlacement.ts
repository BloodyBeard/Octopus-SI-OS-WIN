export type WidgetRect = { x: number; y: number; width: number; height: number };
type PositionSpace = { left: number; top: number; right: number; bottom: number };

export function placeVoiceWidget(
  stage: { width: number; height: number },
  widget: { width: number; height: number },
  occupied: WidgetRect[],
  protectedAreas: WidgetRect[] = [],
  random: () => number = Math.random,
): { x: number; y: number } {
  const gap = 8;
  const domain = { left: gap, top: gap, right: Math.max(gap, stage.width - widget.width - gap), bottom: Math.max(gap, stage.height - widget.height - gap) };
  const center = { x: stage.width * 0.35, y: stage.height * 0.3, width: stage.width * 0.3, height: stage.height * 0.4 };
  const protectedRects = [center, ...protectedAreas];
  let spaces = [domain];
  for (const rect of [...protectedRects, ...occupied]) {
    // Forbidden top-left coordinates account for the whole widget, not just its anchor.
    const blocked = { left: rect.x - widget.width - gap, top: rect.y - widget.height - gap, right: rect.x + rect.width + gap, bottom: rect.y + rect.height + gap };
    spaces = spaces.flatMap((space) => subtractSpace(space, blocked));
  }
  const area = (space: PositionSpace) => (space.right - space.left) * (space.bottom - space.top);
  const totalArea = spaces.reduce((sum, space) => sum + area(space), 0);
  if (totalArea > 0) {
    let choice = random() * totalArea;
    const space = spaces.find((candidate) => (choice -= area(candidate)) < 0) ?? spaces[spaces.length - 1];
    return samplePosition(space, random);
  }

  // A crowded or narrow canvas may have no free rectangle. Keep the widget inside
  // it and minimize overlap, with the conversation and composer taking priority.
  const candidates = [
    { x: domain.left, y: domain.top }, { x: domain.right, y: domain.top },
    { x: domain.left, y: domain.bottom }, { x: domain.right, y: domain.bottom },
    ...Array.from({ length: 48 }, () => samplePosition(domain, random)),
  ];
  const score = (position: { x: number; y: number }) => {
    const rect = { ...widget, ...position };
    return protectedRects.reduce((sum, other) => sum + overlap(rect, other) * 4, 0)
      + occupied.reduce((sum, other) => sum + overlap(rect, other), 0);
  };
  let selected = candidates[Math.floor(random() * candidates.length)];
  let bestScore = score(selected);
  for (const candidate of candidates) {
    const nextScore = score(candidate);
    if (nextScore < bestScore) { selected = candidate; bestScore = nextScore; }
  }
  return selected;
}

function samplePosition(space: PositionSpace, random: () => number) {
  return { x: space.left + random() * (space.right - space.left), y: space.top + random() * (space.bottom - space.top) };
}

function subtractSpace(space: PositionSpace, blocked: PositionSpace): PositionSpace[] {
  const left = Math.max(space.left, blocked.left), right = Math.min(space.right, blocked.right);
  const top = Math.max(space.top, blocked.top), bottom = Math.min(space.bottom, blocked.bottom);
  if (left >= right || top >= bottom) return [space];
  return [
    { ...space, right: left }, { ...space, left: right },
    { left, right, top: space.top, bottom: top }, { left, right, top: bottom, bottom: space.bottom },
  ].filter((part) => part.right > part.left && part.bottom > part.top);
}

function overlap(a: WidgetRect, b: WidgetRect) {
  return Math.max(0, Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x))
    * Math.max(0, Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y));
}
