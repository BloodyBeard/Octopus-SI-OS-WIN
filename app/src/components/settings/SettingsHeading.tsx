import type { ReactNode } from "react";

export function SettingsHeading({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
}) {
  return (
    <div className="settings-section-heading pb-8">
      <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
        <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
          <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
            <h1 className="min-w-0 break-words text-default heading-lg font-normal">{title}</h1>
            {description ? <div className="text-base text-balance text-secondary">{description}</div> : null}
          </div>
          {actions ? <div className="flex max-w-full shrink-0 items-center gap-2">{actions}</div> : null}
        </div>
      </header>
    </div>
  );
}
