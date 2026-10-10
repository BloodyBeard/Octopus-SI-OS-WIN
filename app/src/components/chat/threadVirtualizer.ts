export type ThreadVirtualizerEntry = {
  turnKey: string;
  estimatedHeightPx?: number;
};

export type ThreadVirtualizerLayout = {
  bottomOffsetsPx: number[];
  heightsPx: number[];
  topOffsetsPx: number[];
  totalHeightPx: number;
  turnIndexByKey: Map<string, number>;
  turnKeys: string[];
};

export type ThreadVirtualizerRange = {
  startIndex: number;
  endIndex: number;
};

const DEFAULT_ESTIMATED_TURN_HEIGHT_PX = 280;

export function buildThreadVirtualizerLayout({
  entries,
  gapPx,
  measuredHeightsByKey,
}: {
  entries: readonly ThreadVirtualizerEntry[];
  gapPx: number;
  measuredHeightsByKey: Readonly<Record<string, number>>;
}): ThreadVirtualizerLayout {
  const heightsPx: number[] = [];
  const topOffsetsPx: number[] = [];
  const turnIndexByKey = new Map<string, number>();
  const turnKeys: string[] = [];
  let totalHeightPx = 0;

  for (const [index, entry] of entries.entries()) {
    const height = measuredHeightsByKey[entry.turnKey]
      ?? entry.estimatedHeightPx
      ?? DEFAULT_ESTIMATED_TURN_HEIGHT_PX;
    turnIndexByKey.set(entry.turnKey, index);
    turnKeys.push(entry.turnKey);
    topOffsetsPx.push(totalHeightPx);
    heightsPx.push(height);
    totalHeightPx += height;
    if (index < entries.length - 1) totalHeightPx += gapPx;
  }

  return {
    bottomOffsetsPx: topOffsetsPx.map((offset, index) => (
      totalHeightPx - offset - (heightsPx[index] ?? 0)
    )),
    heightsPx,
    topOffsetsPx,
    totalHeightPx,
    turnIndexByKey,
    turnKeys,
  };
}

export function threadVirtualizerRange({
  distanceFromBottomPx,
  layout,
  overscanCount,
  viewportHeightPx,
}: {
  distanceFromBottomPx: number;
  layout: ThreadVirtualizerLayout;
  overscanCount: number;
  viewportHeightPx: number;
}): ThreadVirtualizerRange {
  if (layout.turnKeys.length === 0) return { startIndex: 0, endIndex: 0 };
  const viewportStartFromBottom = Math.min(
    Math.max(0, distanceFromBottomPx),
    layout.totalHeightPx,
  );
  const viewportEndFromBottom = Math.min(
    viewportStartFromBottom + Math.max(0, viewportHeightPx),
    layout.totalHeightPx,
  );
  const firstVisible = firstBottomOffsetBelow(layout.bottomOffsetsPx, viewportEndFromBottom);
  const lastVisible = firstTurnEndingAbove(
    layout.bottomOffsetsPx,
    layout.heightsPx,
    viewportStartFromBottom,
  );
  return {
    startIndex: Math.max(0, firstVisible - overscanCount),
    endIndex: Math.min(
      layout.turnKeys.length,
      Math.max(lastVisible, firstVisible + 1) + overscanCount,
    ),
  };
}

export function rangeAnchoredToTurn({
  anchorKey,
  layout,
  previousRange,
}: {
  anchorKey: string;
  layout: ThreadVirtualizerLayout;
  previousRange: ThreadVirtualizerRange;
}): ThreadVirtualizerRange | null {
  const index = layout.turnIndexByKey.get(anchorKey);
  if (index === undefined) return null;
  return {
    startIndex: index,
    endIndex: Math.min(
      layout.turnKeys.length,
      index + previousRange.endIndex - previousRange.startIndex,
    ),
  };
}

export function distanceFromBottomAnchoredToTurn({
  anchorKey,
  distanceFromBottomPx,
  nextLayout,
  previousLayout,
}: {
  anchorKey: string;
  distanceFromBottomPx: number;
  nextLayout: ThreadVirtualizerLayout;
  previousLayout: ThreadVirtualizerLayout;
}): number | null {
  const previousIndex = previousLayout.turnIndexByKey.get(anchorKey);
  const nextIndex = nextLayout.turnIndexByKey.get(anchorKey);
  if (previousIndex === undefined || nextIndex === undefined) return null;
  const previousTopFromBottom = (previousLayout.bottomOffsetsPx[previousIndex] ?? 0)
    + (previousLayout.heightsPx[previousIndex] ?? 0);
  const nextTopFromBottom = (nextLayout.bottomOffsetsPx[nextIndex] ?? 0)
    + (nextLayout.heightsPx[nextIndex] ?? 0);
  return Math.max(0, distanceFromBottomPx + nextTopFromBottom - previousTopFromBottom);
}

export function distanceFromBottomForTurn({
  layout,
  turnKey,
  viewportHeightPx,
}: {
  layout: ThreadVirtualizerLayout;
  turnKey: string;
  viewportHeightPx: number;
}): number | null {
  const index = layout.turnIndexByKey.get(turnKey);
  if (index === undefined) return null;
  return Math.max(
    0,
    (layout.bottomOffsetsPx[index] ?? 0)
      - viewportHeightPx / 2
      + (layout.heightsPx[index] ?? 0) / 2,
  );
}

function firstBottomOffsetBelow(offsets: readonly number[], threshold: number): number {
  let low = 0;
  let high = offsets.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if ((offsets[middle] ?? 0) < threshold) high = middle;
    else low = middle + 1;
  }
  return low;
}

function firstTurnEndingAbove(
  offsets: readonly number[],
  heights: readonly number[],
  threshold: number,
): number {
  let low = 0;
  let high = offsets.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if ((offsets[middle] ?? 0) + (heights[middle] ?? 0) <= threshold) high = middle;
    else low = middle + 1;
  }
  return low;
}
