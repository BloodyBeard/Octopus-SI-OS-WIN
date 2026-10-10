import { useLayoutEffect, useRef, useState, type CSSProperties, type HTMLAttributes } from "react";
import { useShouldReduceMotion } from "../../appearance";
import "./octopus-marquee.css";

type MarqueeGeometry = { contentWidth: number; scrollDistance: number; speed: number };

const marqueeTrajectory = [[0, "0.0000"], [0.04512847900390626, "0.0557"], [0.08868408203125, "0.1101"], [0.13073455810546875, "0.1633"], [0.17134765625000004, "0.2152"], [0.21059112548828118, "0.2658"], [0.24853271484375006, "0.3151"], [0.28524017333984375, "0.3629"], [0.32078125, "0.4094"], [0.3552236938476562, "0.4543"], [0.38863525390625, "0.4978"], [0.42108367919921874, "0.5397"], [0.45263671875, "0.5801"], [0.4833621215820313, "0.6188"], [0.51332763671875, "0.6559"], [0.5426010131835937, "0.6913"], [0.5712499999999999, "0.7250"], [0.5993423461914062, "0.7569"], [0.62694580078125, "0.7871"], [0.6541281127929689, "0.8154"], [0.6809570312499998, "0.8418"], [0.7075003051757812, "0.8663"], [0.7338256835937501, "0.8889"], [0.7600009155273436, "0.9095"], [0.7860937500000001, "0.9281"], [0.8121719360351564, "0.9447"], [0.83830322265625, "0.9591"], [0.8645553588867189, "0.9715"], [0.89099609375, "0.9816"], [0.9176931762695314, "0.9896"], [0.9447143554687499, "0.9954"], [0.9721273803710938, "0.9988"], [1, "1.0000"]] as const;

export function marqueeMotion(distance: number, speed: number): CSSProperties {
  if (!Number.isFinite(distance) || distance < 0 || !Number.isFinite(speed) || speed <= 0) {
    throw new RangeError("Marquee motion requires a finite distance and positive speed");
  }
  const travel = distance / speed;
  const total = .35 + travel;
  const stops = marqueeTrajectory.map(([phase, position]) => position + " " + ((.35 + phase * travel) / total * 100).toFixed(4) + "%");
  return {
    "--marquee-duration": total.toFixed(3) + "s",
    "--marquee-scroll-distance": distance + "px",
    "--marquee-scroll-timing": "linear(0.0000 0.0000%, " + stops.join(", ") + ")",
  } as CSSProperties;
}

export function OctopusMarquee({ children, className = "", animateOnGroupHover = false, ...props }: HTMLAttributes<HTMLSpanElement> & {
  animateOnGroupHover?: boolean;
}) {
  const viewport = useRef<HTMLSpanElement>(null);
  const track = useRef<HTMLSpanElement>(null);
  const content = useRef<HTMLSpanElement>(null);
  const [geometry, setGeometry] = useState<MarqueeGeometry | null>(null);
  const reduceMotion = useShouldReduceMotion();

  useLayoutEffect(() => {
    const outer = viewport.current;
    const inner = content.current;
    const scrolling = track.current;
    if (!outer || !inner || !scrolling) return;
    const measure = () => {
      const contentWidth = inner.getBoundingClientRect().width || inner.scrollWidth;
      const viewportWidth = outer.getBoundingClientRect().width || outer.clientWidth;
      if (viewportWidth <= 0 || contentWidth <= viewportWidth) {
        setGeometry(null);
        return;
      }
      const style = getComputedStyle(inner);
      const font = Number.parseFloat(style.fontSize);
      const emSpeed = Number.parseFloat(style.getPropertyValue("--marquee-speed-em-per-second"));
      const speed = (Number.isFinite(font) && font > 0 ? font : 13) * (Number.isFinite(emSpeed) && emSpeed > 0 ? emSpeed : 2);
      const scrollDistance = contentWidth - viewportWidth;
      setGeometry((current) => current?.contentWidth === contentWidth && current.scrollDistance === scrollDistance && current.speed === speed
        ? current
        : { contentWidth, scrollDistance, speed });
    };
    const observer = new ResizeObserver(measure);
    observer.observe(outer);
    observer.observe(scrolling);
    observer.observe(inner);
    measure();
    return () => observer.disconnect();
  }, [children]);

  const motion = geometry && !reduceMotion ? marqueeMotion(geometry.scrollDistance, geometry.speed) : undefined;
  return (
    <span
      {...props}
      className={`octopus-marquee${animateOnGroupHover ? " octopus-marquee-on-group-hover" : ""} ${className}`}
      data-marquee-animatable={Boolean(motion) || undefined}
      data-marquee-overflowing={Boolean(geometry) || undefined}
      data-marquee-text=""
      draggable={false}
      ref={viewport}
    >
      <span className={`octopus-marquee-clip${geometry ? " octopus-marquee-overflowing" : ""}`} style={motion}>
        <span className={`octopus-marquee-track${motion ? " octopus-marquee-scrolling" : ""}`} ref={track}>
          <span className="octopus-marquee-content" data-marquee-content="" ref={content}>{children}</span>
        </span>
      </span>
    </span>
  );
}
