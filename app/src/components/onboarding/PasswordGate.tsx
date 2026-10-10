import { useState } from "react";
import { configureClientAccess, unlockClientAccess } from "../../clientAccess";
import type { OctopusClientAccessStatus } from "../../electron";
import { appToast } from "../../toasts";

const MIN_LENGTH = 8;

type Props = {
  mode: "set" | "unlock";
  onDone: (status: OctopusClientAccessStatus) => void;
  onLaunchingChange?: (launching: boolean) => void;
};

export function PasswordGate({ mode, onDone, onLaunchingChange }: Props) {
  const [value, setValue] = useState("");
  const [show, setShow] = useState(false);
  const [forgotOpen, setForgotOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [launching, setLaunching] = useState(false);

  const isSet = mode === "set";
  const tooShort = value.trim().length < MIN_LENGTH;

  async function submit() {
    if (busy || launching) return;
    if (isSet) {
      if (tooShort) {
        appToast.danger(`Use at least ${MIN_LENGTH} characters.`);
        return;
      }
      setBusy(true);
      try {
        finish(await configureClientAccess(value));
      } catch (failure) {
        appToast.danger(failure instanceof Error ? failure.message : String(failure));
      } finally {
        setBusy(false);
      }
      return;
    }
    setBusy(true);
    try {
      finish(await unlockClientAccess(value));
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  function finish(status: OctopusClientAccessStatus) {
    setLaunching(true);
    onLaunchingChange?.(true);
    window.setTimeout(() => onDone(status), 520);
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  }

  return (
    <div className="welcome-lock-form">
      <span className="welcome-tag">{isSet ? "Set a password" : "Locked"}</span>
      <h1 id="welcome-title" className="welcome-title">
        {isSet ? "Lock the controls" : "Welcome back"}
      </h1>
      <p className="welcome-body">
        {isSet
          ? "Set a password for this device. Anyone who opens Octopus could otherwise drive the agent — this gates the controls. It's stored only here, never sent to your runtime or Gateway."
          : "Enter the Administrator or Test password. Each profile opens a separate client data boundary on this device."}
      </p>

      <div className="welcome-lock-actions">
        <div className="welcome-lock-row">
          <input
            autoComplete={isSet ? "new-password" : "current-password"}
            className="welcome-lock-input"
            type={show ? "text" : "password"}
            spellCheck={false}
            autoFocus
            placeholder={isSet ? "Choose a password" : "Password"}
            value={value}
            onChange={(event) => setValue(event.target.value)}
            onKeyDown={onKeyDown}
            aria-label={isSet ? "Choose a password" : "Password"}
          />
          <button
            className="welcome-lock-eye"
            type="button"
            onClick={() => setShow((s) => !s)}
            aria-label={show ? "Hide password" : "Show password"}
            tabIndex={-1}
          >
            {show ? <EyeOff /> : <Eye />}
          </button>
        </div>
        <button
          className="welcome-lock-go"
          type="button"
          onClick={submit}
          disabled={busy || (isSet && tooShort)}
          aria-label={isSet ? "Set password and enter" : "Unlock"}
        >
          <KeyIcon />
        </button>
      </div>

      <div className="welcome-lock-forgot-wrap">
        <button className="welcome-lock-forgot" type="button" onClick={() => setForgotOpen((o) => !o)}>
          Forgot password?
        </button>
        {forgotOpen ? (
          <p className="welcome-lock-forgot-info">
            The password lives only on this device and can't be recovered here. Reset the host credential file to
            configure a new one; this screen has no recovery bypass.
          </p>
        ) : null}
      </div>
    </div>
  );
}

function KeyIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="25"
      height="25"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <circle cx="15.4" cy="8.6" r="4.3" />
      <path d="M12.4 11.6 4.8 19.2" />
      <path d="M7.7 16.3 10 18.6" />
      <path d="M5.7 18.3 7.6 20.2" />
    </svg>
  );
}

function Eye() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" focusable="false">
      <path fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z" />
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" strokeWidth="1.7" />
    </svg>
  );
}

function EyeOff() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" focusable="false">
      <path fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" d="M4 4l16 16M9.9 5.6A9.5 9.5 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-2.8 3.4M6.3 7.7A16 16 0 0 0 2.5 12S6 18.5 12 18.5a9 9 0 0 0 3-.5M9.5 9.5a3 3 0 0 0 4.2 4.2" />
    </svg>
  );
}
