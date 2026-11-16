import { useState, type ReactNode } from "react";

interface SettingsSectionProps {
  title: string;
  summary?: string;
  defaultOpen?: boolean;
  children: ReactNode;
}

export default function SettingsSection({
  title,
  summary,
  defaultOpen = true,
  children,
}: SettingsSectionProps) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <details
      className="settings-group"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="settings-group-summary">
        <span className="settings-group-heading">
          <strong className="settings-group-title">{title}</strong>
          {summary && <span className="settings-group-meta">{summary}</span>}
        </span>
        <span className="settings-group-chevron" aria-hidden="true">
          ▾
        </span>
      </summary>
      <div className="settings-group-body">{children}</div>
    </details>
  );
}
