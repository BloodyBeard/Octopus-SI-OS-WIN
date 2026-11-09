import type { ReactNode } from "react";
import type { GestureInteraction, NavigationDirection } from "../../airHandEngine";
import { OcButton } from "../../design-system/components";
import { airHandGestures, emitAirHandGesture, emitGestureInteraction } from "../../gestureShortcuts";

const DIRECTIONS: readonly NavigationDirection[] = ["left", "up", "down", "right"];

const SEQUENCES: readonly { label: string; interactions: readonly GestureInteraction[] }[] = [
  {
    label: "Select",
    interactions: [
      { type: "press", side: "right" },
      { type: "release", side: "right", outcome: "select", dx: 0, dy: 0 },
    ],
  },
  {
    label: "Carry",
    interactions: [
      { type: "press", side: "right" },
      { type: "grab", side: "right", phase: "begin", dx: 0.04, dy: 0 },
      { type: "grab", side: "right", phase: "move", dx: 0.09, dy: 0 },
      { type: "grab", side: "right", phase: "move", dx: 0.14, dy: 0 },
      { type: "release", side: "right", outcome: "drop", dx: 0.14, dy: 0 },
    ],
  },
  {
    label: "Throw",
    interactions: [
      { type: "press", side: "right" },
      { type: "grab", side: "right", phase: "begin", dx: 0.05, dy: 0 },
      { type: "grab", side: "right", phase: "move", dx: 0.2, dy: 0 },
      { type: "release", side: "right", outcome: "fling", dx: 0.2, dy: 0 },
    ],
  },
  {
    label: "Two-hand resize",
    interactions: [
      { type: "transform", phase: "begin", scale: 1, dx: 0, dy: 0 },
      { type: "transform", phase: "move", scale: 1.2, dx: 0, dy: 0 },
      { type: "transform", phase: "move", scale: 1.45, dx: 0, dy: 0 },
      { type: "transform", phase: "end", scale: 1.45, dx: 0, dy: 0 },
    ],
  },
];

export function GestureSimulateButtons({ disabled, leading }: { disabled: boolean; leading?: ReactNode }) {
  return (
    <div className="gesture-simulate-row">
      {leading}
      {DIRECTIONS.map((direction) => (
        <OcButton
          disabled={disabled}
          key={direction}
          onClick={() => emitGestureInteraction({ type: "navigate", side: "right", direction }, "test")}
          size="sm"
          variant="secondary"
        >
          Focus {direction}
        </OcButton>
      ))}
      {SEQUENCES.map((sequence) => (
        <OcButton
          disabled={disabled}
          key={sequence.label}
          onClick={() => {
            for (const interaction of sequence.interactions) emitGestureInteraction(interaction, "test");
          }}
          size="sm"
          variant="secondary"
        >
          {sequence.label}
        </OcButton>
      ))}
      {airHandGestures.map((gesture) => (
        <OcButton
          disabled={disabled}
          key={gesture.id}
          onClick={() => emitAirHandGesture(gesture.id, "test")}
          size="sm"
          variant="secondary"
        >
          {gesture.label}
        </OcButton>
      ))}
    </div>
  );
}
