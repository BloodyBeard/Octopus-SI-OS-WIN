import { useId, useRef } from "react";
import type { OctopusChatgptAccount } from "../../electron";
import { OcButton, OcDialog, OcIconButton } from "../../design-system/components";
import "./account.css";

export function AccountLogoutDialog({ account, open, busy, onClose, onConfirm }:
  { account: OctopusChatgptAccount | null; open: boolean; busy: boolean; onClose: () => void; onConfirm: () => void }) {
  const titleId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);
  return <OcDialog open={open} onClose={onClose} busy={busy} size="narrow" ariaLabelledBy={titleId} initialFocusRef={cancelRef}>
    <form className="account-logout-form" onSubmit={(event) => {
      event.preventDefault();
      if (busy || !account?.signedIn) return;
      onClose();
      onConfirm();
    }}>
      <header className="account-logout-header">
        <h2 id={titleId}>Log out?</h2>
        {account?.email ? <p title={account.email}>Signed in as {account.email}</p> : null}
        <OcIconButton className="account-logout-close" glyph="close" label="Close dialog" onClick={onClose} disabled={busy} variant="ghost" />
      </header>
      <div className="account-logout-description">
        <p>You’ll need to sign in again to keep using ChatGPT in Octopus.</p>
        <p>Applications and Homes using <code>{account?.credentialHome ?? "this native credential location"}</code> share this sign-in. Logging out removes the saved sign-in there; new ChatGPT requests need sign-in again.</p>
        <p>Your Octopus Home and local Administrator access are kept.</p>
      </div>
      <footer className="account-logout-actions">
        <OcButton buttonRef={cancelRef} variant="ghost" onClick={onClose} disabled={busy}>Cancel</OcButton>
        <OcButton type="submit" tone="danger" disabled={busy || !account?.signedIn}>Log out</OcButton>
      </footer>
    </form>
  </OcDialog>;
}
