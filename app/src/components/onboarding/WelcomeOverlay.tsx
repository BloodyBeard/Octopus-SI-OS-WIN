import { useEffect, useMemo, useRef, useState } from "react";
import { OctopusDotCloud } from "./OctopusDotCloud";
import { MorphVideoDotCloud } from "./MorphVideoDotCloud";
import { Glyph, type GlyphName } from "../IconGlyphs";

type Props = {
  visitCount: number;
  onComplete: () => void;
};

type StepKind = "hero" | "manifesto" | "primitives" | "preview";

type Step = {
  kind: StepKind;
  cta: string;
};

const welcomeSteps: Step[] = [
  { kind: "hero", cta: "Get started" },
  { kind: "preview", cta: "Agree & continue" },
  { kind: "manifesto", cta: "Continue" },
  { kind: "primitives", cta: "Continue" },
];

const primitives: Array<{ glyph: GlyphName; name: string; copy: string }> = [
  { glyph: "interfaces", name: "Interfaces", copy: "Declared ports for providers, channels, sensors, workers, and tool rooms." },
  { glyph: "ee", name: "Worker environments", copy: "Where tools act: a workspace, browser, desktop, or container." },
  { glyph: "sites", name: "Artifacts", copy: "Data crossing an interface: a message, file reference, tool call, or result." },
  { glyph: "binding", name: "Policy & bindings", copy: "What a run may use, which interfaces it can reach, and what context it can read." },
  { glyph: "topology", name: "Goals & runs", copy: "Environments hold goals and permissions. Runs record work and pending checkpoints." },
  { glyph: "descriptor", name: "Descriptors", copy: "JSON that declares capabilities and connects them to the system." },
];

const HERO_SRC = "./octopus-providers.png";
const GITHUB_URL = "https://github.com/miuuyy/Octopus-SI-OS";
const X_URL = "https://x.com/miuuyy";
const DOCS_URL = "https://github.com/miuuyy/Octopus-SI-OS#readme";

type IdeaTheme = { heading: string; body: string; src: string };

const SHELL_MARK_SRC = "./mark-shell.png";
const SHELL_MARK_VIDEO_SRC = "./mark-shell.mp4";

const IDEA_THEMES: IdeaTheme[] = [
  {
    heading: "Connected through interfaces",
    body:
      "System is everything Octopus can act on. World is everything outside it. They meet at declared interfaces: a model provider, a microphone, a chat channel. Each exchanges artifacts through the same boundary. Adding a connection means installing its descriptor and driver code, then granting access to it. The kernel stays the same.",
    src: "./onboarding/artifact-protocol.png",
  },
  {
    heading: "Add the software you need",
    body:
      "Install the base once, then choose applications and drivers for your work. Capabilities are declared in JSON, with executable code where needed. If a piece is missing, an authorized coding worker can build it and register it through ordinary file tools. Later runs can use that capability when their bindings and permissions allow it.",
    src: "./onboarding/descriptor-evolution.png",
  },
  {
    heading: "Work can wait and continue",
    body:
      "A run records work inside an environment with a goal and permissions. It can wait at a checkpoint and continue when the required input arrives. Tools act in worker environments: a workspace, a browser, a desktop, or a container. The run's progress and pending requirements remain on disk while it waits.",
    src: "./onboarding/worker-environment.png",
  },
  {
    heading: "Check the result",
    body:
      "A checkpoint can require an artifact to pass a check before work continues. The verifier performs that check; the runtime records a mark when it passes. For example, a build check can establish that a build succeeded. The mark links the result to its evidence and proves only what that verifier checked.",
    src: "./onboarding/verified-mark.png",
  },
  {
    heading: "Permissions follow the work",
    body:
      "Each run has a specific grant: the intersection of its worker's maximum permissions, its environment hierarchy, and its authored bindings. Octopus recomputes that grant whenever an artifact arrives. The model sees the tools and context available to it; the runtime and tool rooms enforce the boundaries. Inner environments can only narrow the permissions around them.",
    src: "./onboarding/policy-formation.png",
  },
  {
    heading: "A record you can inspect",
    body:
      "Messages, tool calls, results, and memory cross interfaces as artifacts. A manifest identifies the content and where it lives. The ledger records the movements, including refusals and failed checks. You can trace what reached a run, which boundary it crossed, and what evidence supports a result.",
    src: "./onboarding/artifact-ledger.png",
  },
  {
    heading: "One place for your work",
    body:
      "Use Home for an ongoing conversation by text or voice. Organize work into Projects, configure Agents, and give them Tasks. Applications connect providers, services, and context; Drivers provide the places where tools act. Dashboard shows the underlying environments, runs, and movements. Your configuration and work records stay in your local Home directory.",
    src: SHELL_MARK_SRC,
  },
];

const IDEA_SRCS = IDEA_THEMES.map((theme) => theme.src);

export function WelcomeOverlay({ visitCount, onComplete }: Props) {
  const [stepIndex, setStepIndex] = useState(0);
  const [heroAssembled, setHeroAssembled] = useState(false);
  const replayKey = useRef(0);
  const steps = useMemo(() => welcomeSteps, []);
  const step = steps[stepIndex];
  const isHero = step.kind === "hero";
  const isFirst = stepIndex === 0;
  const isLast = stepIndex === steps.length - 1;
  const footerHidden = isHero && !heroAssembled;

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Enter" || event.key === "ArrowRight") {
        event.preventDefault();
        advance();
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        back();
      } else if (event.key === "Escape" && !isLast) {
        event.preventDefault();
        onComplete();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  function goTo(index: number) {
    if (steps[index].kind === "hero") {
      replayKey.current += 1;
      setHeroAssembled(false);
    }
    setStepIndex(index);
  }

  function advance() {
    if (stepIndex < steps.length - 1) {
      goTo(stepIndex + 1);
      return;
    }
    onComplete();
  }

  function back() {
    if (stepIndex > 0) goTo(stepIndex - 1);
  }

  return (
    <main className="welcome" data-visit-count={visitCount} data-step={step.kind} aria-labelledby="welcome-title">
      <header className="welcome-top">
        <div className="welcome-brand">
          <img className="welcome-brand-mark" src="./octopus-logo.png" alt="" />
          <span className="welcome-brand-name">Octopus&nbsp;OS</span>
        </div>
        {!isLast ? (
          <button className="welcome-skip" type="button" onClick={onComplete}>
            Skip
          </button>
        ) : (
          <span className="welcome-skip welcome-skip-spacer" aria-hidden="true" />
        )}
      </header>

      {isHero ? (
        <HeroStage
          assembled={heroAssembled}
          onAssembled={() => setHeroAssembled(true)}
          replayKey={replayKey.current}
        />
      ) : (
        <div className="welcome-stage" key={stepIndex} data-kind={step.kind}>
          {step.kind === "manifesto" ? <IdeaStep /> : null}
          {step.kind === "primitives" ? <PrimitivesStep /> : null}
          {step.kind === "preview" ? <PreviewStep /> : null}
        </div>
      )}

      <footer className="welcome-footer" data-hidden={footerHidden ? "true" : "false"}>
        <div className="welcome-footer-side welcome-footer-left">
          {!isFirst ? (
            <button className="welcome-back" type="button" onClick={back}>
              Back
            </button>
          ) : null}
        </div>
        <div className="welcome-step-count" aria-hidden="true">
          <span className="welcome-step-now">{String(stepIndex + 1).padStart(2, "0")}</span>
          <span className="welcome-step-sep">/</span>
          <span className="welcome-step-total">{String(steps.length).padStart(2, "0")}</span>
        </div>
        <div className="welcome-footer-side welcome-footer-right">
          <button className="welcome-next" type="button" onClick={advance}>
            {step.cta}
          </button>
        </div>
      </footer>
    </main>
  );
}

function HeroStage({
  assembled,
  onAssembled,
  replayKey,
}: {
  assembled: boolean;
  onAssembled: () => void;
  replayKey: number;
}) {
  return (
    <div className="welcome-hero-stage">
      <div className="welcome-hero-bg">
        <OctopusDotCloud src={HERO_SRC} framing="bottom" onAssembled={onAssembled} replayKey={replayKey} />
        <div className="welcome-hero-veil" aria-hidden="true" />
      </div>
      <div className={`welcome-hero-copy${assembled ? " is-in" : ""}`}>
        <span className="welcome-tag">Research Preview</span>
        <h1 id="welcome-title" className="welcome-title welcome-title-hero">
          Welcome to Octopus&nbsp;OS
        </h1>
        <p className="welcome-body welcome-hero-body">
          An operating system for agents. Talk to it, give it work, and choose the applications and tools it can use.
        </p>
      </div>
    </div>
  );
}

function IdeaStep() {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const progressRef = useRef(0);
  const sectionIndexRef = useRef(0);
  const wheelDeltaRef = useRef(0);
  const wheelLockedRef = useRef(false);
  const wheelResetTimerRef = useRef<number | null>(null);
  const wheelUnlockTimerRef = useRef<number | null>(null);

  function computeProgress() {
    const el = scrollRef.current;
    if (!el) return;
    const sections = Array.from(el.querySelectorAll<HTMLElement>(".welcome-idea-section"));
    if (sections.length > 0) {
      const progress = Math.max(0, Math.min(sections.length - 1, el.scrollTop / Math.max(1, el.clientHeight)));
      progressRef.current = progress;
      sectionIndexRef.current = Math.round(progress);
    }
  }

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      if (event.ctrlKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
      event.preventDefault();
      if (wheelUnlockTimerRef.current != null) window.clearTimeout(wheelUnlockTimerRef.current);
      wheelUnlockTimerRef.current = window.setTimeout(() => {
        wheelLockedRef.current = false;
      }, 260);
      if (wheelLockedRef.current) return;
      wheelDeltaRef.current += event.deltaY;
      if (wheelResetTimerRef.current != null) window.clearTimeout(wheelResetTimerRef.current);
      wheelResetTimerRef.current = window.setTimeout(() => {
        wheelDeltaRef.current = 0;
      }, 140);
      if (Math.abs(wheelDeltaRef.current) < 28) return;
      const sections = Array.from(el.querySelectorAll<HTMLElement>(".welcome-idea-section"));
      const direction = wheelDeltaRef.current > 0 ? 1 : -1;
      const nextIndex = Math.max(0, Math.min(sections.length - 1, sectionIndexRef.current + direction));
      wheelDeltaRef.current = 0;
      if (nextIndex === sectionIndexRef.current) return;
      sectionIndexRef.current = nextIndex;
      wheelLockedRef.current = true;
      el.scrollTo({ top: nextIndex * el.clientHeight, behavior: "smooth" });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    computeProgress();
    return () => {
      el.removeEventListener("wheel", onWheel);
      if (wheelResetTimerRef.current != null) window.clearTimeout(wheelResetTimerRef.current);
      if (wheelUnlockTimerRef.current != null) window.clearTimeout(wheelUnlockTimerRef.current);
    };
  }, []);

  return (
    <section className="welcome-idea">
      <div className="welcome-idea-left">
        <span className="welcome-tag">The idea</span>
        <div className="welcome-idea-scroll" ref={scrollRef} onScroll={computeProgress}>
          {IDEA_THEMES.map((theme, i) => (
            <div className="welcome-idea-section" key={theme.heading}>
              <h2 className="welcome-idea-heading" {...(i === 0 ? { id: "welcome-title" } : {})}>
                {theme.heading}
              </h2>
              <p className="welcome-body">{theme.body}</p>
            </div>
          ))}
        </div>
      </div>
      <div className="welcome-idea-art" aria-hidden="true">
        <MorphVideoDotCloud
          srcs={IDEA_SRCS}
          videoSrc={SHELL_MARK_VIDEO_SRC}
          progressRef={progressRef}
          particleScale={1.6}
          monochrome
          sparkle
          className="welcome-video-cloud"
        />
      </div>
    </section>
  );
}

function PrimitivesStep() {
  return (
    <section className="welcome-primitives">
      <span className="welcome-tag">How it works</span>
      <h1 id="welcome-title" className="welcome-title">
        A few primitives, composed
      </h1>
      <p className="welcome-body welcome-body-wide">
        A small kernel connects these primitives. Applications and drivers provide the capabilities you use.
      </p>
      <div className="welcome-grid">
        {primitives.map((item, index) => (
          <article className="welcome-card" key={item.name} style={{ "--i": index } as React.CSSProperties}>
            <Glyph name={item.glyph} size={20} className="welcome-card-glyph" />
            <h2 className="welcome-card-title">{item.name}</h2>
            <p className="welcome-card-copy">{item.copy}</p>
          </article>
        ))}
      </div>
      <a className="welcome-textlink" href={DOCS_URL} target="_blank" rel="noreferrer">
        Read the documentation
        <Glyph name="chevronDown" size={14} className="welcome-textlink-arrow" />
      </a>
    </section>
  );
}

function PreviewStep() {
  return (
    <section className="welcome-centered">
      <span className="welcome-tag">Before you start</span>
      <h1 id="welcome-title" className="welcome-title">
        This is a research preview
      </h1>
      <p className="welcome-body">
        Octopus is early and moving fast. Expect rough edges, breaking changes, and ideas still finding their shape. It
        runs locally, with your own ChatGPT account.
      </p>
      <div className="welcome-support">
        <span className="welcome-support-label">Follow along or support the project</span>
        <div className="welcome-support-links">
          <a className="welcome-pill" href={GITHUB_URL} target="_blank" rel="noreferrer">
            <GitHubIcon />
            Star on GitHub
          </a>
          <a className="welcome-pill" href={X_URL} target="_blank" rel="noreferrer">
            <XIcon />
            Follow on X
          </a>
        </div>
      </div>
    </section>
  );
}

function GitHubIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path
        fill="currentColor"
        d="M12 2.3c-5.46 0-9.9 4.42-9.9 9.88 0 4.37 2.84 8.08 6.78 9.39.5.09.68-.22.68-.48 0-.24-.01-1.02-.02-1.85-2.76.6-3.34-1.17-3.34-1.17-.45-1.15-1.1-1.45-1.1-1.45-.9-.62.07-.61.07-.61 1 .07 1.52 1.02 1.52 1.02.88 1.51 2.31 1.08 2.88.82.09-.64.35-1.08.63-1.32-2.2-.25-4.52-1.1-4.52-4.9 0-1.08.39-1.97 1.02-2.66-.1-.25-.44-1.26.1-2.62 0 0 .83-.27 2.72 1.02.79-.22 1.64-.33 2.48-.33.84 0 1.69.11 2.48.33 1.89-1.29 2.72-1.02 2.72-1.02.54 1.36.2 2.37.1 2.62.64.69 1.02 1.58 1.02 2.66 0 3.81-2.32 4.65-4.53 4.9.36.31.68.92.68 1.86 0 1.34-.01 2.42-.01 2.75 0 .27.18.58.69.48a9.9 9.9 0 0 0 6.77-9.39c0-5.46-4.43-9.88-9.9-9.88Z"
      />
    </svg>
  );
}

function XIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path
        fill="currentColor"
        d="M13.65 10.63 20.12 3h-1.53l-5.62 6.62L8.49 3H3.32l6.78 10.01L3.32 21h1.53l5.93-6.99L15.52 21h5.17l-7.04-10.37Zm-2.1 2.47-.69-1L5.4 4.16h2.36l4.41 6.41.69 1 5.73 8.33h-2.36l-4.68-6.8Z"
      />
    </svg>
  );
}
