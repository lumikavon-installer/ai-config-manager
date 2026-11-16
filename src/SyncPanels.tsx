import { useEffect, useRef, useState } from "react";
import SettingsSection from "./SettingsSection";
import { messages } from "./i18n";
import type { Language, RepositoryProbeResult, SyncItem, SyncPolicy, SyncProvider, SyncRecord, SyncView } from "./types";

type Copy = (typeof messages)[Language];
export type SyncTab = "target" | "sync" | "applications" | "sync_history" | "automation";
export type ApplicationDraft = {
  app_id: string;
  app_name: string;
  config_id: string;
  label: string;
  socket_path: string;
  path: string;
};
export const EMPTY_APPLICATION: ApplicationDraft = {
  app_id: "", app_name: "", config_id: "", label: "", socket_path: "", path: "/config",
};
export type PolicyDraft = {
  auto_sync_enabled: boolean;
  sync_interval_minutes: string;
  history_retention_days: string;
};
export const DEFAULT_SYNC_POLICY: SyncPolicy = {
  auto_sync_enabled: true, sync_interval_minutes: 10, history_retention_days: 30,
};
export function policyDraftOf(policy = DEFAULT_SYNC_POLICY): PolicyDraft {
  return {
    auto_sync_enabled: policy.auto_sync_enabled,
    sync_interval_minutes: String(policy.sync_interval_minutes),
    history_retention_days: String(policy.history_retention_days),
  };
}
// Render protocol codes only. Never render an arbitrary error message or payload.
const RECORD_ERROR_CODES = new Set([
  "IO", "LOCK_TIMEOUT", "UNSAFE_PATH", "TOO_LARGE", "INVALID_CONFIG", "CONFLICT",
  "HISTORY_PENDING", "COMMIT_UNCERTAIN", "KEY_MISSING", "DECRYPT_FAILED", "ENCRYPT_FAILED",
  "SYNC_BUSY", "SYNC_NOT_CONFIGURED", "SYNC_DISABLED", "SYNC_REPO", "SYNC_TOKEN", "SYNC_PASSWORD",
  "SYNC_PATH", "SYNC_FILE_TOO_LARGE", "SYNC_DUPLICATE", "SYNC_ITEM_MISSING", "SYNC_REQUIRED",
  "SYNC_VAULT", "SYNC_CONFIG", "SYNC_BUNDLE", "SYNC_STATE", "SYNC_ENCRYPT", "SYNC_DECRYPT",
  "SYNC_POLICY", "SYNC_APPLICATION", "SYNC_APPLICATION_ID", "SYNC_APPLICATION_INTERFACE",
  "SYNC_APPLICATION_NETWORK", "SYNC_APPLICATION_TIMEOUT", "SYNC_APPLICATION_RESPONSE",
  "SYNC_APPLICATION_CONFLICT", "SYNC_APPLY", "SYNC_REMOTE", "SYNC_HISTORY",
  "SYNC_REGISTRATION", "SYNC_INTERFACE", "SYNC_CALLBACK", "SYNC_LOCAL_CHANGED", "SYNC_INTERRUPTED",
  "SYNC_APPLICATION_OFFLINE",
  "GITHUB_AUTH", "GITHUB_FORBIDDEN", "GITHUB_REPO_NOT_FOUND", "GITHUB_CONFLICT",
  "GITHUB_REJECTED", "GITHUB_SERVER", "GITHUB_NETWORK", "GITHUB_TIMEOUT", "GITHUB_RATE_LIMIT",
  "GIT_UNAVAILABLE", "GIT_REPO", "GIT_BRANCH", "GIT_READ", "GIT_WRITE", "GIT_CONFLICT",
  "GIT_TIMEOUT", "GIT_CLEANUP", "GIT_FAILED",
]);
export function repositoryCode(code: string) {
  return code === "OK" || RECORD_ERROR_CODES.has(code) ? code : "UNKNOWN";
}
export function deferredReason(reason: string, t: Copy) {
  return reason === "SYNC_APPLICATION_OFFLINE" ? t.syncDeferredOffline
    : reason === "SYNC_CALLBACK" ? t.syncDeferredCallback
      : reason === "SYNC_LOCAL_CHANGED" ? t.syncDeferredChanged
        : t.syncDeferredUnknown;
}

function ItemActions({ item, name, t, busy, canRun, onRun, onAutoSync, onToggle, onRemove }: {
  item: SyncItem; name: string; t: Copy; busy: boolean;
  canRun: boolean;
  onRun: (appId?: string) => void;
  onAutoSync: (item: SyncItem, enabled: boolean) => void;
  onToggle: (item: SyncItem, enabled: boolean) => void;
  onRemove: (item: SyncItem) => void;
}) {
  return <>
    <label className="sync-toggle">
      <input type="checkbox" checked={item.enabled} disabled={busy}
        aria-label={`${t.syncEnabled}: ${name}`}
        onChange={(event) => onToggle(item, event.target.checked)} />
      {t.syncEnabled}
    </label>
    {item.application && <>
      <label className="sync-toggle">
        <input type="checkbox" checked={item.auto_sync ?? true} disabled={busy}
          aria-label={`${t.autoSync}: ${name}`}
          onChange={(event) => onAutoSync(item, event.target.checked)} />
        {t.autoSync}
      </label>
      <button type="button" disabled={busy || !canRun || !item.enabled}
        aria-label={`${t.syncNow}: ${name}`}
        onClick={() => onRun(item.application!.app_id)}>{t.syncNow}</button>
    </>}
    {item.kind !== "ai_config" && <button type="button" disabled={busy}
      aria-label={`${t.syncRemove}: ${name}`} onClick={() => onRemove(item)}>{t.syncRemove}</button>}
  </>;
}

function ApplicationDetails({ item, t }: { item: SyncItem; t: Copy }) {
  const application = item.application;
  return <div>
    <strong>{application?.app_name ?? item.label}</strong>
    <small>{t.applicationLabel}: {item.label}</small>
    {application && <>
      <small>{t.applicationId}: {application.app_id} · {t.applicationConfigId}: {application.config_id}</small>
      <small>{t.applicationSocket}: <code>{application.sync_interface.socket_path}</code></small>
      <small>{t.applicationPath}: <code>{application.sync_interface.path}</code></small>
    </>}
  </div>;
}

type Props = {
  tab: SyncTab;
  language: Language;
  sync: SyncView | null;
  busy: boolean;
  repo: string; token: string; encryptionSecret: string;
  provider: SyncProvider; gitBranch: string;
  onProvider: (value: SyncProvider) => void; onGitBranch: (value: string) => void;
  repositoryProbe: RepositoryProbeResult | null; repositoryTesting: boolean; onTestRepository: () => void;
  repositoryCreating: boolean; onCreateRepository: () => void;
  targetDirty: boolean;
  onRepo: (value: string) => void; onToken: (value: string) => void;
  onPassword: (value: string) => void;
   onSaveTarget: () => void; onRefresh: () => void; onRun: (appId?: string) => void;
   onAutoSync: (item: SyncItem, enabled: boolean) => void;
  onToggle: (item: SyncItem, enabled: boolean) => void; onRemove: (item: SyncItem) => void;
  warningText: (value: string) => string;
  application: ApplicationDraft; applicationDirty: boolean;
  onApplication: (value: ApplicationDraft) => void; onRegister: () => Promise<string | null>;
  policy: PolicyDraft; policyDirty: boolean;
  onPolicy: (value: PolicyDraft) => void; onSavePolicy: () => void; onCleanup: () => void;
  recordsBusy: boolean; onRefreshRecords: () => void;
};

export default function SyncPanels(props: Props) {
  const { tab, language, sync, busy, onToggle, onRemove } = props;
  const t = messages[language];
  if (!sync) return <div className="card">
    <p>{t.syncUnavailable}</p>
    <button onClick={props.onRefresh} disabled={busy}>{t.reload}</button>
  </div>;
  if (tab === "applications") return <ApplicationsPanel {...props} />;
  if (tab === "sync_history") return <section className="card sync-card" aria-label={t.settingsSyncHistory} aria-busy={props.recordsBusy}>
    <div className="card-top"><h2>{t.settingsSyncHistory}</h2>
      <button disabled={busy || props.recordsBusy} onClick={props.onRefreshRecords}>{t.syncRecordsRefresh}</button></div>
    <p className="sync-intro">{t.syncRecordsIntro}</p>
    {!sync.records?.length && <p className="sync-message">{t.noSyncRecords}</p>}
    <ul className="sync-record-list">
      {(sync.records ?? []).slice(0, 100).map((record) => <SyncRecordCard key={record.id} record={record} t={t} />)}
    </ul>
  </section>;
  if (tab === "automation") {
    const policy = props.policy;
    return <section className="card sync-card" aria-label={t.settingsAutomation}>
      <div className="card-top"><h2>{t.settingsAutomation}</h2>{props.policyDirty && <span className="badge warning">{t.unsaved}</span>}</div>
      <p className="sync-intro">{t.automationIntro}</p>
      <form noValidate onSubmit={(event) => { event.preventDefault(); props.onSavePolicy(); }}>
        <fieldset disabled={busy}>
          <label className="toggle-row"><strong>{t.autoSync}</strong>
            <input type="checkbox" role="switch" checked={policy.auto_sync_enabled}
              onChange={(event) => props.onPolicy({ ...policy, auto_sync_enabled: event.target.checked })} /></label>
          <label htmlFor="sync-interval">{t.syncInterval}</label>
          <input id="sync-interval" type="number" min={1} max={1440} step={1}
            value={policy.sync_interval_minutes} aria-describedby="sync-interval-help"
            onChange={(event) => props.onPolicy({ ...policy, sync_interval_minutes: event.target.value })} />
          <small id="sync-interval-help">{t.syncIntervalHelp}</small>
          <label htmlFor="history-retention">{t.historyRetention}</label>
          <input id="history-retention" type="number" min={0} max={3650} step={1}
            value={policy.history_retention_days} aria-describedby="history-retention-help"
            onChange={(event) => props.onPolicy({ ...policy, history_retention_days: event.target.value })} />
          <small id="history-retention-help">{t.historyRetentionHelp}</small>
        </fieldset>
        <div className="actions"><button className="primary" type="submit" disabled={busy}>{t.saveSyncPolicy}</button></div>
      </form>
      <dl className="sync-record-stats"><div><dt>{t.nextSyncAt}</dt>
        <dd>{sync.next_sync_at ? <time dateTime={sync.next_sync_at}>{sync.next_sync_at}</time> : t.syncNotScheduled}</dd></div></dl>
      <div className="actions"><button disabled={busy} onClick={props.onCleanup} aria-describedby="sync-cleanup-help">{t.cleanupSyncHistory}</button></div>
      <small id="sync-cleanup-help">{t.syncCleanupHelp}</small>
      <p className="sync-message">{t.syncConflictPolicy}</p>
    </section>;
  }
  const configured = !!sync.repo && ((sync.provider ?? "github") === "git" || sync.token_present) && sync.encryption_secret_present;
  if (tab === "target") return <section className="card sync-card" aria-label={t.syncTargetGroup}>
    <div className="card-top"><h2>{t.syncTargetGroup}</h2>
      <span className="badge">{configured ? `${sync.repo} · ${t.syncTargetConfigured}` : t.syncTargetUnconfigured}</span>
      {props.targetDirty && <span className="badge warning">{t.unsaved}</span>}</div>
    <p className="sync-intro">{t.syncTargetIntro}</p>
    <fieldset disabled={busy}>
      <label htmlFor="sync-provider">{t.syncProvider}</label>
      <select id="sync-provider" value={props.provider} onChange={(event) => props.onProvider(event.target.value as SyncProvider)}>
        <option value="github">{t.syncProviderGithub}</option><option value="git">{t.syncProviderGit}</option>
      </select>
      {props.provider === "github" && <>
        <label htmlFor="sync-token">{t.githubToken}</label>
        <input id="sync-token" type="password" value={props.token} onChange={(event) => props.onToken(event.target.value)} placeholder={sync.token_present ? t.githubTokenSaved : ""} autoComplete="new-password" spellCheck={false} />
        <small>{t.githubTokenHelp}</small>
      </>}
      <label htmlFor="sync-repo">{props.provider === "github" ? t.githubRepo : t.gitRepo}</label>
      <div className="input-with-action">
        <input id="sync-repo" value={props.repo} onChange={(event) => props.onRepo(event.target.value)} placeholder={props.provider === "github" ? "owner/repo" : "ssh://git@example.com/team/configs.git"} autoComplete="off" spellCheck={false} aria-describedby="sync-repo-help" />
        {props.provider === "github" && <button type="button" disabled={busy || !props.repo.trim()} onClick={props.onCreateRepository}>{props.repositoryCreating ? t.githubCreatingRepository : t.githubCreateRepository}</button>}
      </div>
      <small id="sync-repo-help">{props.provider === "github" ? t.githubRepoHelp : t.gitRepoHelp}</small>
      {props.provider === "github" && <small>{t.githubCreateRepositoryHelp}</small>}
      {props.provider === "git" && <>
        <label htmlFor="sync-branch">{t.gitBranch}</label>
        <input id="sync-branch" value={props.gitBranch} onChange={(event) => props.onGitBranch(event.target.value)} placeholder="main" autoComplete="off" spellCheck={false} aria-describedby="sync-branch-help" />
        <small id="sync-branch-help">{t.gitBranchHelp}</small>
      </>}
      <label htmlFor="sync-secret">{t.syncPassword}</label>
      <input id="sync-secret" type="password" value={props.encryptionSecret} onChange={(event) => props.onPassword(event.target.value)} placeholder={sync.encryption_secret_present ? t.syncPasswordSaved : ""} autoComplete="new-password" spellCheck={false} />
      <small>{t.syncPasswordHelp}</small>
    </fieldset>
    <div className="actions"><button className="primary" disabled={busy} onClick={props.onSaveTarget}>{t.saveSync}</button>
      <button disabled={busy || !props.repo.trim()} onClick={props.onTestRepository} aria-describedby="repository-test-help">{props.repositoryTesting ? t.repositoryTesting : t.repositoryTest}</button>
      <button disabled={busy} onClick={props.onRefresh}>{t.syncRefresh}</button></div>
    <p className="sync-message" id="repository-test-help">{t.repositoryTestHelp}</p>
    {props.repositoryProbe && <div className={`repository-probe ${props.repositoryProbe.ok ? "" : "error"}`} role="status" aria-label={t.repositoryTest}>
      <p>{props.repositoryProbe.ok ? t.repositoryTestSuccess : t.repositoryTestFailure}</p>
      <dl className="sync-record-stats">
        {([[t.repositoryTestRead, props.repositoryProbe.readable], [t.repositoryTestWrite, props.repositoryProbe.writable], [t.repositoryTestCleanup, props.repositoryProbe.cleanup_ok]] as const).map(([label, passed]) => <div key={label}><dt>{label}</dt><dd>{passed ? t.repositoryTestPassed : t.repositoryTestFailed}</dd></div>)}
        <div><dt>{t.syncErrorCode}</dt><dd><code>{repositoryCode(props.repositoryProbe.code)}</code></dd></div>
        <div><dt>{t.repositoryTest}</dt><dd>{props.repositoryProbe.latency_ms} ms</dd></div>
      </dl>
      {!props.repositoryProbe.cleanup_ok && props.repositoryProbe.probe_ref && <p className="system-warning">{t.repositoryCleanupRequired} <code>{props.repositoryProbe.probe_ref}</code></p>}
    </div>}
  </section>;
  const applications = sync.items.filter((item) => item.kind === "application");
  const enabledCount = applications.filter((item) => item.enabled).length;
  return <section className="card sync-card" aria-label={t.sync}>
    <div className="card-top"><h2>{t.sync}</h2>
      <span className={`badge ${sync.status.state === "error" ? "warning" : ""}`}>
        {sync.running || sync.status.state === "running" ? t.syncing : sync.status.state === "success" ? t.syncSuccess : sync.status.state === "error" ? t.syncError : t.syncNever}
      </span></div>
    <p className="sync-intro">{t.syncIntro}</p>
    <p className="sync-intro">{t.syncConflictPolicy}</p>
    {!!sync.warnings?.length && <p className="system-warning" role="alert">{[...new Set(sync.warnings.map(props.warningText))].join(" ")}</p>}
    {!!sync.deferred?.length && <div className="sync-deferred" role="status" aria-label={t.syncDeferredTitle}>
      <strong>{t.syncDeferredTitle}</strong>
      <ul>{sync.deferred.map((item) => <li key={item.id}>
        <span>{item.label}</span>
        <small>{deferredReason(item.reason, t)}</small>
      </li>)}</ul>
      <small>{t.syncDeferredHelp}{sync.next_sync_at && <> · {t.nextSyncAt}: <time dateTime={sync.next_sync_at}>{sync.next_sync_at}</time></>}</small>
    </div>}
    <SettingsSection title={t.syncItems} summary={`${enabledCount} / ${applications.length}`}>
      {!applications.length && <p className="sync-message">{t.noApplicationSync}</p>}
      <ul className="sync-list application-list">{applications.map((item) => {
        const name = `${item.application?.app_name ?? item.label} · ${item.label}`;
        return <li key={item.id}><ApplicationDetails item={item} t={t} />
          <ItemActions item={item} name={name} t={t} busy={busy} canRun={configured && !props.targetDirty && !sync.running}
            onRun={props.onRun} onAutoSync={props.onAutoSync} onToggle={onToggle} onRemove={onRemove} /></li>;
      })}</ul>
      <div className="actions">
        <button className="primary" disabled={busy || !configured || props.targetDirty || sync.running || !enabledCount} onClick={() => props.onRun()}>{t.syncNow}</button></div>
    </SettingsSection>
    <div className="sync-status">
      <div><small>{t.syncLastAt}</small><strong>{sync.status.last_synced_at || "—"}</strong></div>
      <div><small>{t.syncApplicationCount}</small><strong>{enabledCount}</strong></div>
      <div><small>{t.syncRemote}</small><strong>{sync.status.remote_url || sync.status.remote_path}</strong></div>
    </div>
    {sync.status.message && sync.status.state !== "success" && <p className="sync-message">{sync.status.message}</p>}
  </section>;
}

function ApplicationsPanel(props: Props) {
  const { language, sync, busy, onToggle, onRemove } = props;
  const t = messages[language];
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const registerButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const dialog = dialogRef.current!;
    const trigger = registerButton.current;
    dialog.showModal();
    dialog.querySelector("input")?.focus();
    return () => {
      dialog.close();
      if (trigger?.isConnected) trigger.focus();
    };
  }, [open]);
  const applications = sync!.items.filter((item) => item.kind === "application");
  const draft = props.application;
  const fields = [
    ["app_id", t.applicationId], ["app_name", t.applicationName],
    ["config_id", t.applicationConfigId], ["label", t.applicationLabel],
    ["socket_path", t.applicationSocket], ["path", t.applicationPath],
  ] as const;
  async function register() {
    const message = await props.onRegister();
    setError(message);
    if (!message) setOpen(false);
  }
  return <section className="card sync-card" aria-label={t.settingsApplications}>
    <div className="card-top application-heading">
      <div><h2>{t.settingsApplications}</h2><span className="badge">{applications.length}</span>
        {props.applicationDirty && <span className="badge warning">{t.unsaved}</span>}</div>
      <button ref={registerButton} className="primary" disabled={busy} onClick={() => { setError(null); setOpen(true); }}>
        {t.openApplicationRegistration}
      </button>
    </div>
    <p className="sync-intro">{t.applicationsIntro}</p>
    {!applications.length && <p className="sync-message">{t.noApplications}</p>}
    <ul className="sync-list application-list">
      {applications.map((item) => {
        const application = item.application;
        const name = `${application?.app_name ?? item.label} · ${item.label}`;
        return <li key={item.id}>
          <ApplicationDetails item={item} t={t} />
          <ItemActions item={item} name={name} t={t} busy={busy}
            canRun={!!sync!.repo && ((sync!.provider ?? "github") === "git" || sync!.token_present) && sync!.encryption_secret_present && !props.targetDirty && !sync!.running}
            onRun={props.onRun} onAutoSync={props.onAutoSync} onToggle={onToggle} onRemove={onRemove} />
        </li>;
      })}
    </ul>
    {open && <dialog ref={dialogRef} className="application-dialog" aria-labelledby="application-dialog-title"
      aria-describedby="application-interface-help" aria-busy={busy}
      onCancel={(event) => { event.preventDefault(); if (!busy) setOpen(false); }}>
      <form noValidate onSubmit={(event) => { event.preventDefault(); if (!busy) void register(); }}>
        <div className="card-top"><h2 id="application-dialog-title">{t.openApplicationRegistration}</h2>
          {props.applicationDirty && <span className="badge warning">{t.unsaved}</span>}</div>
        <fieldset disabled={busy} className="sync-form-grid">
          {fields.map(([field, label]) => <div key={field}>
            <label htmlFor={`application-${field}`}>{label}</label>
            <input id={`application-${field}`} value={draft[field]} autoComplete="off" spellCheck={false}
              aria-describedby={field === "socket_path" || field === "path" ? "application-interface-help" : undefined}
              onChange={(event) => { setError(null); props.onApplication({ ...draft, [field]: event.target.value }); }} />
          </div>)}
        </fieldset>
        <small id="application-interface-help">{t.applicationInterfaceHelp}</small>
        {error && <p className="status error" role="alert">{error}</p>}
        <div className="actions">
          <button type="button" disabled={busy} onClick={() => setOpen(false)}>{t.cancelApplicationRegistration}</button>
          <button className="primary" disabled={busy} type="submit">{busy ? t.saving : t.registerApplication}</button>
        </div>
      </form>
    </dialog>}
  </section>;
}

function SyncRecordCard({ record, t }: { record: SyncRecord; t: Copy }) {
  const trigger = record.trigger === "scheduled" ? t.syncTriggerScheduled : record.trigger === "agent" ? t.syncTriggerAgent : t.syncTriggerManual;
  return <li><article className="sync-record" aria-label={`${record.started_at} · ${trigger}`}>
    <div className="card-top"><time dateTime={record.started_at}>{record.started_at}</time>
      <span className={`badge ${record.state === "error" ? "warning" : ""}`}>{t.syncRecordResult}: {record.state === "success" ? t.syncRecordSuccess : t.syncRecordError}</span></div>
    <dl className="sync-record-stats">
      <div><dt>{t.syncRecordCompleted}</dt><dd><time dateTime={record.completed_at}>{record.completed_at}</time></dd></div>
      <div><dt>{t.source}</dt><dd>{trigger}</dd></div>
      {record.app_id && <div><dt>{t.applicationId}</dt><dd>{record.app_id}</dd></div>}
      <div><dt>{t.syncItemCount}</dt><dd>{record.item_count}</dd></div>
      <div><dt>{t.syncUploaded}</dt><dd>{record.uploaded_count}</dd></div>
      <div><dt>{t.syncDownloaded}</dt><dd>{record.downloaded_count}</dd></div>
      <div><dt>{t.syncConflicts}</dt><dd>{record.conflict_count}</dd></div>
      <div><dt>{t.syncErrorCode}</dt><dd><code>{record.error_code ? RECORD_ERROR_CODES.has(record.error_code) ? record.error_code : t.syncUnknownError : "—"}</code></dd></div>
    </dl>
  </article></li>;
}
