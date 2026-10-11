import { useState } from "react";
import { useChatgptAccount } from "../../account/useChatgptAccount";
import { useChatgptUsage } from "../../account/useChatgptUsage";
import { accountPlanLabel } from "../../account/chatgptUsageModel";
import { OcButton, OcSettingsRow } from "../../design-system/components";
import { AccountLogoutDialog } from "../account/AccountLogoutDialog";
import { AccountSection, AccountUsageSections } from "../account/AccountUsage";
import { SettingsHeading } from "./SettingsHeading";
import { settingsTargetProps } from "./settingsCatalog";

export function AccountSettingsSection() {
  const state = useChatgptAccount();
  const account = state.account;
  const usage = useChatgptUsage(account);
  const [logoutOpen, setLogoutOpen] = useState(false);
  const busy = state.action !== "idle";
  const status = state.loading ? "Checking account…" : !account ? "Unavailable"
    : account.signedIn ? accountPlanLabel(account.planType) : "Signed out";
  async function refresh() {
    const refreshed = await state.refresh?.();
    if (refreshed?.signedIn) await usage.refresh?.();
  }
  return <section className="settings-section account-settings">
    <SettingsHeading title="Account" actions={<OcButton disabled={busy || state.loading || usage.loading} onClick={() => void refresh()}>Refresh</OcButton>} />
    {state.error ? <div className="account-inline-notice account-error" role="alert"><span>{state.error}</span>
      <OcButton disabled={busy || state.loading} onClick={() => void state.refresh?.()}>Retry account check</OcButton>
    </div> : null}
    <AccountSection title="Account" {...settingsTargetProps("account.identity")}>
      <div className="account-row-list">
        <OcSettingsRow label="Status" control={<span className="account-value">{status}</span>} />
        <OcSettingsRow label="Email" control={<span className="account-value">{state.loading ? "Loading…" : account?.email ?? "Unavailable"}</span>} />
      </div>
    </AccountSection>
    <div {...settingsTargetProps("account.usage")} className="account-usage-sections">
      <AccountUsageSections state={usage} signedIn={Boolean(account?.signedIn)} accountUnavailable={!account && !state.loading} accountLoading={state.loading && !account} />
    </div>
    <AccountSection title="Sign-in" {...settingsTargetProps("account.logout")}>
      {state.signInStatusError ? <div className="account-inline-notice account-error" role="alert"><span>{state.signInStatusError}</span></div> : null}
      {state.action === "waiting" || state.action === "cancelling" ? <div className="account-section-actions">
        {state.signInStatusError ? <OcButton onClick={() => void state.retrySignInStatus?.()}>Retry sign-in check</OcButton> : null}
        <OcButton disabled={state.action === "cancelling"} onClick={() => void state.cancel?.()}>Cancel sign-in</OcButton>
      </div> : null}
      <div className="account-row-list">
        <OcSettingsRow label={account?.signedIn ? "Log out of ChatGPT" : "Sign in with ChatGPT"} control={account?.signedIn
          ? <OcButton disabled={busy || state.loading} onClick={() => setLogoutOpen(true)}>{state.action === "signing-out" ? "Logging out…" : "Log out"}</OcButton>
          : <OcButton disabled={busy || state.loading || !state.signIn} onClick={() => void state.signIn?.()}>{state.action === "starting" ? "Opening browser…" : "Continue with ChatGPT"}</OcButton>} />
      </div>
    </AccountSection>
    <AccountLogoutDialog account={account} open={logoutOpen} busy={busy || state.loading} onClose={() => setLogoutOpen(false)} onConfirm={() => void state.logout?.()} />
  </section>;
}
