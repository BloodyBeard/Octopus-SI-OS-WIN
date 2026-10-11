import type { HTMLAttributes, ReactNode } from "react";
import type { ChatgptUsageState } from "../../account/chatgptUsageState";
import { accountRemainingLabel, accountResetTime, accountUsageGroups } from "../../account/chatgptUsageModel";
import { OcButton, OcSettingsRow } from "../../design-system/components";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import "./account.css";

type UsageViewState = ChatgptUsageState & { refresh?: () => Promise<unknown> };

export function AccountSection({ title, description, actions, children, className = "", ...props }:
  Omit<HTMLAttributes<HTMLElement>, "title"> & { title: ReactNode; description?: ReactNode; actions?: ReactNode }) {
  return <section {...props} className={`account-section ${className}`}>
    <header className={`account-section-header${description ? " has-description" : ""}`}>
      <div className="account-section-heading"><h2>{title}</h2>{description ? <p>{description}</p> : null}</div>
      {actions ? <div className="account-section-actions">{actions}</div> : null}
    </header>
    <div className="account-section-content">{children}</div>
  </section>;
}

export function AccountRemainingProgress({ remaining, label, suffix = "left" }: { remaining: number | null; label: string; suffix?: string }) {
  return <div className="account-usage-progress">
    {remaining !== null ? <progress max={100} value={remaining} aria-label={label} /> : null}
    <span>{accountRemainingLabel(remaining, suffix)}</span>
  </div>;
}

function UsageNotice({ state }: { state: UsageViewState }) {
  return state.error ? <div className="account-inline-notice account-error" role="alert">
    <span>Usage is unavailable. {state.error}</span>
    <OcButton onClick={() => void state.refresh?.()}>Retry</OcButton>
  </div> : <div className="account-inline-notice" role="status"><OctopusSpinner className="icon-xs" /><span>Loading usage…</span></div>;
}

export function AccountUsageSections({ state, signedIn, accountUnavailable = false, accountLoading = false }:
  { state: UsageViewState; signedIn: boolean; accountUnavailable?: boolean; accountLoading?: boolean }) {
  if (!signedIn || !state.value) return <AccountSection title="General usage limits">
    {signedIn && (state.loading || state.error) ? <UsageNotice state={state} /> : <p className="account-section-footer" role="status">
      {accountLoading ? "Checking account…" : accountUnavailable ? "Usage is unavailable until your account can be checked."
        : signedIn ? "No usage limits were reported for this account." : "Sign in with ChatGPT to view usage."}
    </p>}
  </AccountSection>;
  const groups = accountUsageGroups(state.value);
  if (!groups.length) return <AccountSection title="General usage limits"><p className="account-section-footer">No usage limits were reported for this account.</p></AccountSection>;
  return <>{groups.map((group) => <AccountSection key={group.key}
    title={group.limit.limitName ? `${group.label} usage limits` : "General usage limits"}
    actions={group.limit.rateLimitReachedType || group.limit.spendControlReached ? <span className="account-limit-reached">Limit reached</span> : undefined}>
    <div className="account-row-list">
      {group.windows.filter((row) => row.reported).map((row) => <OcSettingsRow key={row.key} label={row.label}
        description={accountResetTime(row.resetsAt, row.durationMins)}
        control={<AccountRemainingProgress remaining={row.remaining} label={`${group.label}: ${row.label} remaining`} />} />)}
      {group.limit.individualLimit ? <OcSettingsRow label="Monthly usage limit"
        description={<>{accountResetTime(group.limit.individualLimit.resetsAt, 43_200)} · {group.limit.individualLimit.used} of {group.limit.individualLimit.limit} credits used</>}
        control={<AccountRemainingProgress remaining={group.limit.individualLimit.remainingPercent} label={`${group.label}: monthly usage remaining`} suffix="remaining" />} /> : null}
      {group.limit.credits ? <OcSettingsRow label="Credit balance" control={<span className="account-value">
        {group.limit.credits.unlimited ? "Unlimited" : group.limit.credits.balance ?? (group.limit.credits.hasCredits ? "Unavailable" : "None")}
      </span>} /> : null}
      {!group.windows.some((row) => row.reported) && !group.limit.individualLimit && !group.limit.credits
        ? <p className="account-inline-notice">No usage limits were reported for this account.</p> : null}
    </div>
    <p className="account-section-footer">Shared by applications and Homes using this ChatGPT account.
      {state.loading ? <span role="status"> Refreshing usage…</span> : <span> Updated {new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(new Date(state.value!.observedAtMs))}</span>}
    </p>
  </AccountSection>)}</>;
}
