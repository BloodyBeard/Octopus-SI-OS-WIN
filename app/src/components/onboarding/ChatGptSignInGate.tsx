import { useEffect, useRef, useState } from "react";
import { recordChatgptSignIn } from "../../onboarding/chatgptSignIn";
import { appToast } from "../../toasts";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import openaiLogo from "../../assets/openai-logo.svg?raw";

type Phase = "checking" | "idle" | "waiting";

export function ChatGptSignInGate({ onDone }: { onDone: () => void }) {
  const [phase, setPhase] = useState<Phase>("checking");
  const loginId = useRef("");
  const finished = useRef(false);

  function finish() {
    if (finished.current) return;
    finished.current = true;
    recordChatgptSignIn();
    onDone();
  }

  useEffect(() => {
    let active = true;
    void window.octopusTransport!.chatgptAccount().then((account) => {
      if (!active) return;
      if (account.signedIn) finish();
      else setPhase("idle");
    }).catch((error) => {
      if (!active) return;
      appToast.danger(error instanceof Error ? error.message : String(error));
      setPhase("idle");
    });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (phase !== "waiting") return;
    const timer = window.setInterval(() => {
      const transport = window.octopusTransport!;
      void transport.chatgptSignInStatus(loginId.current).then(async (status) => {
        if (status.state === "pending") return;
        window.clearInterval(timer);
        if (status.state === "completed" && (await transport.chatgptAccount()).signedIn) {
          finish();
          return;
        }
        appToast.danger(status.error || "ChatGPT sign-in did not complete.");
        setPhase("idle");
      }).catch((error) => {
        window.clearInterval(timer);
        appToast.danger(error instanceof Error ? error.message : String(error));
        setPhase("idle");
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [phase]);

  async function signIn() {
    if (phase !== "idle") return;
    try {
      loginId.current = (await window.octopusTransport!.chatgptSignInStart()).loginId;
      setPhase("waiting");
    } catch (error) {
      appToast.danger(error instanceof Error ? error.message : String(error));
    }
  }

  function cancel() {
    void window.octopusTransport!.chatgptSignInCancel(loginId.current).catch(() => undefined);
    setPhase("idle");
  }

  if (phase === "checking") {
    return (
      <main className="welcome">
        <OctopusLoadingState className="app-loading-state" label="Opening Octopus" />
      </main>
    );
  }

  return (
    <main className="welcome" aria-labelledby="welcome-title">
      <header className="welcome-top">
        <div className="welcome-brand">
          <img className="welcome-brand-mark" src="./octopus-logo.png" alt="" />
          <span className="welcome-brand-name">Octopus&nbsp;OS</span>
        </div>
      </header>
      <div className="welcome-stage">
        <section className="welcome-centered">
          <span className="welcome-tag">Account</span>
          <h1 id="welcome-title" className="welcome-title">
            {phase === "waiting" ? "Finish signing in" : "Sign in with ChatGPT"}
          </h1>
          <p className="welcome-body">
            {phase === "waiting"
              ? "Complete sign-in in your browser. Octopus continues as soon as your account is confirmed."
              : "Octopus runs on your ChatGPT account. No API key is needed."}
          </p>
          <div className="welcome-support">
            <button className="welcome-next welcome-signin" type="button" onClick={() => void signIn()} disabled={phase === "waiting"}>
              <span aria-hidden="true" className="welcome-signin-mark" dangerouslySetInnerHTML={{ __html: openaiLogo }} />
              {phase === "waiting" ? "Waiting for browser…" : "Continue with ChatGPT"}
            </button>
            {phase === "waiting" ? (
              <button className="welcome-back" type="button" onClick={cancel}>Cancel</button>
            ) : null}
          </div>
        </section>
      </div>
    </main>
  );
}
