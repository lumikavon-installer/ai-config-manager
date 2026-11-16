export type Language = "zh" | "en";
export type EndpointKind = "openai" | "embedding" | "laya" | "cosyvoice";
export type ConfigView = {
  exists: boolean;
  base_url: string;
  model: string;
  key_present: boolean;
  encrypted: boolean;
  revision: string;
  endpoint?: EndpointKind;
  configured?: boolean;
};
export type Settings = {
  silent_start: boolean;
  close_to_tray: boolean;
  language: Language;
  autostart_default_applied?: boolean;
  autostart: boolean;
  version: string;
  platform: string;
  tray_available: boolean;
  autostart_known?: boolean;
  warnings?: string[];
  sync?: SyncView;
};
export type ProbeResult = {
  ok: boolean;
  status: number | null;
  endpoint: string;
  latency_ms: number;
  code: string;
};
export type SyncItem = {
  id: string;
  label: string;
  kind: "ai_config" | "file" | "application";
  path: string | null;
  enabled: boolean;
  auto_sync?: boolean;
  application?: {
    app_id: string;
    app_name: string;
    config_id: string;
    sync_interface: { socket_path: string; path: string };
  } | null;
};
export type SyncApplicationRequest = {
  app_id: string;
  app_name: string;
  config_id: string;
  label: string;
  sync_interface: { socket_path: string; path: string };
  enabled: boolean;
};
export type SyncPolicy = {
  auto_sync_enabled: boolean;
  sync_interval_minutes: number;
  history_retention_days: number;
};
export type SyncRecord = {
  id: string;
  app_id?: string | null;
  started_at: string;
  completed_at: string;
  trigger: "manual" | "agent" | "scheduled";
  state: "success" | "error";
  repo: string;
  item_count: number;
  uploaded_count: number;
  downloaded_count: number;
  conflict_count: number;
  error_code: string | null;
  remote_sha: string | null;
};
export type SyncStatus = {
  state: "never" | "running" | "success" | "error";
  last_synced_at: string | null;
  message: string;
  item_count: number;
  remote_path: string;
  remote_sha: string | null;
  remote_url: string | null;
};
export type SyncView = {
  provider?: SyncProvider;
  git_branch?: string;
  repo: string;
  token_present: boolean;
  encryption_secret_present: boolean;
  items: SyncItem[];
  status: SyncStatus;
  running: boolean;
  warnings: string[];
  policy?: SyncPolicy;
  records?: SyncRecord[];
  next_sync_at?: string | null;
  deferred?: DeferredItem[];
};
export type SyncProvider = "github" | "git";
export type DeferredItem = {
  id: string;
  label: string;
  kind: "ai_config" | "file" | "application";
  reason: string;
};
export type RepositoryProbeResult = {
  ok: boolean;
  readable: boolean;
  writable: boolean;
  cleanup_ok: boolean;
  code: string;
  latency_ms: number;
  probe_ref: string | null;
};
export type CreateRepositoryResult = {
  repo: string;
  url: string;
  private: boolean;
};
export type HistoryEntry = {
  id: string;
  timestamp: string;
  diff: { field: string; before: string | null; after: string | null }[];
  sha256: string;
  source: string;
};
