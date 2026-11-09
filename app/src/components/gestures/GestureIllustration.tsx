import "./gestureIllustration.css";

export type GestureArtId = "point" | "pinch" | "carry" | "throw" | "resize" | "swipe_left" | "swipe_right" | "fist";

type HandShape = "open" | "point" | "pinch" | "fist";

const FINGERS = [
  { id: "index", x: 17, extended: { y: 12, height: 24 }, curled: { y: 23, height: 13 } },
  { id: "middle", x: 24.5, extended: { y: 8, height: 28 }, curled: { y: 22, height: 14 } },
  { id: "ring", x: 32, extended: { y: 11, height: 25 }, curled: { y: 23, height: 13 } },
  { id: "little", x: 39.5, extended: { y: 17, height: 19 }, curled: { y: 25, height: 11 } },
] as const;

const LABELS: Record<GestureArtId, string> = {
  point: "A pointing hand moving right while the highlight steps across three buttons",
  pinch: "Thumb and index finger touching and letting go, pressing a button",
  carry: "A pinched hand carrying a card to a target on the right",
  throw: "A pinched hand flicking a card away and opening as it lets go",
  resize: "Two pinched hands pulling apart while the box between them grows",
  swipe_left: "An open hand flicking quickly to the left",
  swipe_right: "An open hand flicking quickly to the right",
  fist: "A closed fist held still while a ring fills around it",
};

export function GestureIllustration({ gesture, size = "small" }: { gesture: GestureArtId; size?: "small" | "large" }) {
  return (
    <svg aria-label={LABELS[gesture]} className={`gesture-art is-${gesture} is-${size}`} role="img" viewBox="0 0 160 100">
      <Scene gesture={gesture} />
    </svg>
  );
}

function Scene({ gesture }: { gesture: GestureArtId }) {
  if (gesture === "point") {
    return (
      <>
        {[38, 70, 102].map((x) => <rect className="gesture-art__tile" height={14} key={x} rx={4} width={22} x={x} y={8} />)}
        <rect className="gesture-art__focus" height={20} rx={6} width={28} x={35} y={5} />
        <g className="gesture-art__mover"><Hand shape="point" x={50} y={66} scale={0.8} /></g>
      </>
    );
  }
  if (gesture === "pinch") {
    return (
      <>
        <rect className="gesture-art__button" height={18} rx={9} width={44} x={98} y={14} />
        <Hand shape="pinch" x={70} y={58} scale={1} animated />
      </>
    );
  }
  if (gesture === "carry") {
    return (
      <>
        <rect className="gesture-art__zone" height={30} rx={6} width={40} x={112} y={10} />
        <g className="gesture-art__mover">
          <rect className="gesture-art__card" height={22} rx={5} width={30} x={22} y={12} />
          <Hand shape="pinch" x={42} y={62} scale={0.8} />
        </g>
      </>
    );
  }
  if (gesture === "throw") {
    return (
      <>
        <rect className="gesture-art__card is-flying" height={22} rx={5} width={30} x={22} y={12} />
        <g className="gesture-art__mover"><Hand shape="pinch" x={42} y={62} scale={0.8} animated /></g>
      </>
    );
  }
  if (gesture === "resize") {
    return (
      <>
        <rect className="gesture-art__box" height={28} rx={6} width={36} x={62} y={14} />
        <g className="gesture-art__spread is-left"><Hand mirror shape="pinch" x={48} y={64} scale={0.75} /></g>
        <g className="gesture-art__spread is-right"><Hand shape="pinch" x={112} y={64} scale={0.75} /></g>
      </>
    );
  }
  if (gesture === "swipe_left" || gesture === "swipe_right") {
    return (
      <>
        <path className="gesture-art__trail" d="M38 50 h84 M48 40 h64 M48 60 h64" />
        <g className="gesture-art__mover"><Hand shape="open" x={80} y={54} scale={0.85} /></g>
      </>
    );
  }
  return (
    <>
      <circle className="gesture-art__ring-track" cx={80} cy={50} r={38} />
      <circle className="gesture-art__ring" cx={80} cy={50} pathLength={100} r={38} />
      <Hand shape="fist" x={80} y={52} scale={0.9} />
    </>
  );
}

function Hand({ shape, x, y, scale, mirror = false, animated = false }: { shape: HandShape; x: number; y: number; scale: number; mirror?: boolean; animated?: boolean }) {
  return (
    <g className={`gesture-art__hand is-${shape}${animated ? " is-animated" : ""}`} transform={`translate(${x} ${y}) scale(${mirror ? -scale : scale} ${scale}) translate(-32 -32)`}>
      {FINGERS.map((finger) => {
        const extended = shape === "open" || (finger.id === "index" && (shape === "point" || shape === "pinch"));
        const box = extended ? finger.extended : finger.curled;
        return <rect className={`gesture-art__finger is-${finger.id}`} height={box.height} key={finger.id} rx={3.5} width={7} x={finger.x} y={box.y} />;
      })}
      <rect className="gesture-art__palm" height={27} rx={9} width={33} x={15} y={29} />
      <rect className="gesture-art__thumb" height={26} rx={3.5} width={7} x={6.5} y={20} />
      {shape === "pinch" ? <circle className="gesture-art__spark" cx={11.2} cy={20.5} r={5} /> : null}
    </g>
  );
}
