// Sync card UI/functional verification; all IPC and secrets are synthetic.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { RepositoryProbeResult, SyncApplicationRequest, SyncPolicy, SyncProvider, SyncRecord, SyncView } from "./types";

const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(() => true),
  events: new Map<string, () => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ipc);
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: () => void) => {
    ipc.events.set(event, handler);
    return () => ipc.events.delete(event);
  }),
}));

const config = {
  exists: true,
  base_url: "https://example.com/v1",
  model: "model-a",
  key_present: true,
  encrypted: true,
  revision: "revision-1",
};

function syncView(overrides: Partial<SyncView> = {}): SyncView {
  return {
    repo: "alice/configs",
    token_present: true,
    encryption_secret_present: true,
    items: [
      {
        id: "ai-config",
        label: "AI configuration",
        kind: "ai_config",
        path: null,
        enabled: true,
      },
    ],
    status: {
      state: "never",
      last_synced_at: null,
      message: "",
      item_count: 0,
      remote_path: ".ai-config-manager/sync.enc",
      remote_sha: null,
      remote_url: null,
    },
    running: false,
    warnings: [],
    policy: { auto_sync_enabled: false, sync_interval_minutes: 5, history_retention_days: 30 },
    records: [],
    next_sync_at: null,
    deferred: [],
    ...overrides,
  };
}

let sync: SyncView;
let settings: Record<string, unknown>;

beforeEach(() => {
  ipc.events.clear();
  sync = syncView();
  settings = {
    silent_start: true,
    close_to_tray: true,
    language: "zh",
    autostart: true,
    autostart_known: true,
    version: "0.1.1",
    platform: "windows",
    tray_available: true,
    warnings: [],
    sync,
  };
  ipc.isTauri.mockReturnValue(true);
  ipc.invoke.mockReset();
  ipc.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "config_read") return config;
    if (command === "history_list") return [];
    if (command === "settings_read") return { ...settings, sync };
    if (command === "settings_save") {
      settings = { ...settings, ...(args?.settings as object) };
      return { ...settings, sync };
    }
    if (command === "sync_read") return sync;
    if (command === "sync_history") return sync.records ?? [];
    if (command === "sync_register_application") {
      const request = args?.request as SyncApplicationRequest;
      sync = { ...sync, items: [...sync.items, {
        id: "application-editor", kind: "application", path: null, label: request.label,
        enabled: request.enabled,
        application: { app_id: request.app_id, app_name: request.app_name, config_id: request.config_id, sync_interface: request.sync_interface },
      }] };
      return sync;
    }
    if (command === "sync_save_policy") {
      sync = { ...sync, policy: args?.policy as SyncPolicy, next_sync_at: "2026-11-12T12:30:00Z" };
      return sync;
    }
    if (command === "sync_cleanup") {
      sync = { ...sync, records: [] };
      return sync;
    }
    if (command === "sync_save_settings") {
      sync = { ...sync, repo: args?.repo as string, provider: args?.provider as SyncProvider, git_branch: args?.gitBranch as string };
      return sync;
    }
    if (command === "sync_create_repository") {
      const request = args?.request as { repo: string };
      const repo = request.repo.includes("/") ? request.repo : `alice/${request.repo}`;
      sync = { ...sync, repo };
      return { repo, url: `https://github.com/${repo}`, private: true };
    }
    if (command === "sync_test_repository") return {
      ok: true, readable: true, writable: true, cleanup_ok: true, code: "OK", latency_ms: 123, probe_ref: null,
    };
    if (command === "sync_set_application_auto_sync") {
      sync = { ...sync, items: sync.items.map((item) => item.application?.app_id === args?.appId
        ? { ...item, auto_sync: args?.enabled as boolean } : item) };
      return sync;
    }
    if (command === "sync_run" || command === "sync_run_application") {
      sync = {
        ...sync,
        status: {
          ...sync.status,
          state: "success",
          message: "Synced 2 item(s) to GitHub.",
          item_count: 2,
          last_synced_at: "2026-11-04T00:00:00Z",
          remote_url: `https://github.com/${sync.repo}/blob/main/.ai-config-manager/sync.enc`,
        },
      };
      return sync;
    }
    if (command === "sync_set_item_enabled" || command === "sync_remove_item") {
      sync = { ...sync, items: command === "sync_remove_item"
        ? sync.items.filter((item) => item.id !== args?.id)
        : sync.items.map((item) => item.id === args?.id ? { ...item, enabled: args?.enabled as boolean } : item) };
      return sync;
    }
    if (command === "window_control") return;
    throw new Error(`unexpected command: ${command}`);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function openSync() {
  render(<App />);
  await screen.findByDisplayValue(config.base_url);
  await userEvent.click(screen.getByRole("button", { name: /设置/ }));
  await screen.findByLabelText("界面语言");
  await userEvent.click(screen.getByRole("tab", { name: "同步目标" }));
  await screen.findByRole("heading", { name: "同步目标" });
}
async function openTarget() {
  await tab("同步目标");
  expect(screen.getByLabelText("仓库类型")).toBeVisible();
}

describe("configuration sync card", () => {
  it("saves the target and shows status when syncing a registered application", async () => {
    sync.items.push(applicationItem());
    await openSync();
    await openTarget();
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/configs");
    expect(screen.getByLabelText("GitHub Token")).toHaveValue("");
    expect(screen.getByLabelText("GitHub Token")).toHaveAttribute(
      "type",
      "password",
    );
    expect(screen.getByLabelText("加密密码")).toHaveValue("");
    expect(screen.getByLabelText("加密密码")).toHaveAttribute(
      "type",
      "password",
    );
    expect(screen.queryByText("AI 配置")).not.toBeInTheDocument();

    const repo = screen.getByLabelText("GitHub 仓库");
    await userEvent.clear(repo);
    await userEvent.type(repo, "alice/new-configs");
    await userEvent.click(screen.getByRole("button", { name: "保存同步配置" }));
    await waitFor(() =>
      expect(ipc.invoke).toHaveBeenCalledWith("sync_save_settings", {
        provider: "github",
        gitBranch: "main",
        repo: "alice/new-configs",
        githubToken: null,
        encryptionSecret: null,
      }),
    );

    await tab("同步列表");
    expect(screen.getByText("My Editor")).toBeInTheDocument();
    expect(screen.queryByText("AI 配置")).not.toBeInTheDocument();
    expect(screen.getByText("尚未同步")).toBeInTheDocument();
    expect(screen.queryByLabelText("文件路径")).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "立即同步" }));
    await waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith("sync_run"));
    expect((await screen.findAllByText("最近同步成功")).length).toBeGreaterThan(0);
    expect(screen.getByText("2026-11-04T00:00:00Z")).toBeInTheDocument();
    expect(
      screen.getByText(
        "https://github.com/alice/new-configs/blob/main/.ai-config-manager/sync.enc",
      ),
    ).toBeInTheDocument();
  });

  it("shows deferred items with their reason and the retry time", async () => {
    sync = {
      ...sync,
      deferred: [{
        id: "app:offline:settings", label: "Offline settings", kind: "application",
        reason: "SYNC_APPLICATION_OFFLINE",
      }],
      next_sync_at: "2026-11-12T13:00:00Z",
    };
    await openSync();
    await tab("同步列表");
    const panel = screen.getByRole("status", { name: "等待自动重试的同步项" });
    expect(within(panel).getByText("Offline settings")).toBeVisible();
    expect(within(panel).getByText("第三方应用未运行或无法连接")).toBeVisible();
    expect(within(panel).getByText("2026-11-12T13:00:00Z")).toBeVisible();
  });

  it("shows the token above the repository and creates a private repository from the draft", async () => {
    await openSync();
    await openTarget();
    const token = screen.getByLabelText("GitHub Token");
    const repo = screen.getByLabelText("GitHub 仓库");
    expect(
      token.compareDocumentPosition(repo) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    edit("GitHub 仓库", "");
    expect(screen.getByRole("button", { name: "创建仓库" })).toBeDisabled();
    edit("GitHub 仓库", "alice/new-configs");
    await userEvent.click(screen.getByRole("button", { name: "创建仓库" }));
    await waitFor(() =>
      expect(ipc.invoke).toHaveBeenCalledWith("sync_create_repository", {
        request: { repo: "alice/new-configs", github_token: null },
      }),
    );
    expect(await screen.findByText("已创建私有仓库 · alice/new-configs")).toBeInTheDocument();
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/new-configs");
  });

  it("keeps sync drafts across settings tabs and asks before discarding them", async () => {
    await openSync();
    await userEvent.type(screen.getByLabelText("GitHub Token"), "synthetic-token");
    await tab("关于");
    await userEvent.click(screen.getByRole("button", { name: /历史/ }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "返回" }));
    await tab("同步目标");
    expect(screen.getByLabelText("GitHub Token")).toHaveValue("synthetic-token");
  });

  it("shows only registered application sync items and counts their enabled configurations", async () => {
    sync.items.push(
      { id: "local-file", label: "Legacy file", kind: "file", path: "C:\\configs\\legacy.json", enabled: true },
      applicationItem(),
      { ...applicationItem(), id: "disabled-editor", label: "Other settings", enabled: false,
        application: { ...applicationItem().application!, app_name: "Other Editor", config_id: "other", sync_interface: { socket_path: "/home/test/other/sync.sock", path: "/config" } } },
    );
    await openSync();
    await tab("同步列表");
    const panel = screen.getByRole("region", { name: "同步列表" });
    expect(within(panel).getAllByRole("listitem")).toHaveLength(2);
    expect(within(panel).getByText("My Editor")).toBeVisible();
    expect(within(panel).getByText("Other Editor")).toBeVisible();
    expect(within(panel).getByText("1 / 2")).toBeVisible();
    expect(within(panel).getByText(registration.sync_interface.socket_path)).toBeVisible();
    expect(within(panel).queryByText("AI 配置")).not.toBeInTheDocument();
    expect(within(panel).queryByText("Legacy file")).not.toBeInTheDocument();
    expect(within(panel).queryByText("C:\\configs\\legacy.json")).not.toBeInTheDocument();
    expect(within(panel).queryByLabelText("文件路径")).not.toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "添加" })).not.toBeInTheDocument();
    await userEvent.click(within(panel).getByRole("checkbox", { name: "参与同步: My Editor · Editor preferences" }));
    expect(within(panel).getByText("0 / 2")).toBeVisible();
    expect(within(panel).getByRole("button", { name: "立即同步" })).toBeDisabled();
    expect(sync.items.find((item) => item.id === "ai-config")?.enabled).toBe(true);
    expect(sync.items.find((item) => item.id === "local-file")?.enabled).toBe(true);
    await userEvent.click(within(panel).getByRole("button", { name: "移除: Other Editor · Other settings" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_remove_item", { id: "disabled-editor" });
    expect(within(panel).getAllByRole("listitem")).toHaveLength(1);
  });

  it("shows registration guidance and disables sync until an application is registered", async () => {
    await openSync();
    await tab("同步列表");
    expect(screen.getByText(/暂无应用同步项/)).toHaveTextContent("应用注册");
    expect(screen.getByRole("button", { name: "立即同步" })).toBeDisabled();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_set_item_enabled", expect.anything());
    sync = { ...sync, items: [...sync.items, applicationItem()] };
    await act(async () => { ipc.events.get("sync-changed")?.(); });
    expect(await screen.findByText("My Editor")).toBeVisible();
    expect(screen.queryByText(/暂无应用同步项/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "立即同步" })).toBeEnabled();
  });

  it("refreshes socket changes without replacing the typed target or token", async () => {
    await openSync();
    await openTarget();
    await userEvent.type(screen.getByLabelText("GitHub 仓库"), "-draft");
    await userEvent.type(screen.getByLabelText("GitHub Token"), "synthetic-token");
    sync = syncView({ repo: "third-party/configs", status: {
      ...sync.status, state: "success", item_count: 3, last_synced_at: "2026-11-12T00:00:00Z",
    } });
    await act(async () => { ipc.events.get("sync-changed")?.(); });
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/configs-draft");
    expect(screen.getByLabelText("GitHub Token")).toHaveValue("synthetic-token");
    await tab("同步列表");
    expect(await screen.findByText("2026-11-12T00:00:00Z")).toBeInTheDocument();
  });

  it("uses the latest saved target when leaving and returning to settings", async () => {
    await openSync();
    await openTarget();
    await userEvent.clear(screen.getByLabelText("GitHub 仓库"));
    await userEvent.type(screen.getByLabelText("GitHub 仓库"), "alice/new-configs");
    await userEvent.click(screen.getByRole("button", { name: "保存同步配置" }));
    await screen.findByText("同步配置已保存。");
    await userEvent.click(screen.getByRole("button", { name: /历史/ }));
    await userEvent.click(screen.getByRole("button", { name: /设置/ }));
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/new-configs");
  });

  it("separates target settings from the sidebar list and shows configuration status", async () => {
    await openSync();
    const syncPanel = screen.getByRole("tabpanel", { name: "同步目标" });
    expect(within(syncPanel).getByText("alice/configs · 已配置")).toBeVisible();
    expect(within(syncPanel).getByLabelText("GitHub 仓库")).toBeVisible();
    await tab("同步列表");
    expect(screen.getByRole("button", { name: "同步列表" })).toHaveAttribute("aria-current", "page");
    expect(screen.queryByLabelText("文件路径")).not.toBeInTheDocument();
    expect(screen.getByText(/暂无应用同步项/)).toBeVisible();
    expect(screen.queryByLabelText("GitHub 仓库")).not.toBeInTheDocument();

    cleanup();
    sync = syncView({ repo: "", token_present: false, encryption_secret_present: false });
    settings = { ...settings, sync };
    await openSync();
    const unconfiguredPanel = screen.getByRole("tabpanel", { name: "同步目标" });
    expect(within(unconfiguredPanel).getByText("待配置")).toBeVisible();
    expect(within(unconfiguredPanel).getByLabelText("GitHub 仓库")).toBeVisible();
  });
});

describe("repository backends and permission tests", () => {
  it("tests a GitHub draft without saving it or clearing credentials, and reuses the saved token when blank", async () => {
    sync = syncView({ encryption_secret_present: false });
    await openSync();
    await openTarget();
    edit("GitHub 仓库", "alice/draft");
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_test_repository", { request: {
      provider: "github", repo: "alice/draft", git_branch: "main", github_token: null,
    } });
    const result = screen.getByRole("status", { name: "测试仓库读写" });
    expect(result).toHaveTextContent("读取通过");
    expect(result).toHaveTextContent("写入通过");
    expect(result).toHaveTextContent("清理通过");
    expect(result).toHaveTextContent("OK");
    expect(result).toHaveTextContent("123 ms");
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/draft");
    edit("GitHub Token", "synthetic-draft-token");
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
    edit("加密密码", "synthetic-secret");
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    expect(ipc.invoke).toHaveBeenLastCalledWith("sync_test_repository", { request: {
      provider: "github", repo: "alice/draft", git_branch: "main", github_token: "synthetic-draft-token",
    } });
    expect(screen.getByLabelText("GitHub Token")).toHaveValue("synthetic-draft-token");
    expect(screen.getByLabelText("加密密码")).toHaveValue("synthetic-secret");
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_save_settings", expect.anything());
    expect(sync.repo).toBe("alice/configs");
    expect(screen.getByText(/创建并删除临时分支/)).toHaveTextContent("不改动同步分支");
    expect(screen.getByText(/创建并删除临时分支/)).toHaveTextContent("分支保护");
    edit("加密密码", "synthetic-secret-edited");
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
  });

  it.each(["https://git.example/team/configs.git", "ssh://git@git.example/team/configs.git", "git@git.example:team/configs.git", "C:\\repositories\\configs.git"])("tests and saves custom Git target %s without a token", async (repo) => {
    await openSync();
    await openTarget();
    await userEvent.selectOptions(screen.getByLabelText("仓库类型"), "git");
    expect(screen.queryByLabelText("GitHub Token")).not.toBeInTheDocument();
    expect(screen.getByLabelText("同步分支")).toHaveValue("main");
    expect(screen.getByText(/支持 HTTPS、SSH、SCP/)).toHaveTextContent("credential helper 或 SSH agent");
    edit("Git 仓库", repo);
    edit("同步分支", "configs");
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_test_repository", { request: {
      provider: "git", repo, git_branch: "configs", github_token: null,
    } });
    expect(screen.getByRole("status", { name: "测试仓库读写" })).toBeVisible();
    edit("同步分支", "settings");
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "保存同步配置" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_save_settings", {
      provider: "git", repo, gitBranch: "settings", githubToken: null, encryptionSecret: null,
    });
    await screen.findByText("同步配置已保存。");
  });

  it("enables custom Git sync with no GitHub token and protects branch edits", async () => {
    sync = syncView({ provider: "git", git_branch: "main", repo: "/home/test/configs.git", token_present: false });
    sync.items.push(applicationItem());
    await openSync();
    expect(screen.getByLabelText("Git 仓库")).toBeVisible();
    await tab("同步列表");
    expect(screen.getByRole("button", { name: "立即同步" })).toBeEnabled();
    await userEvent.click(screen.getByRole("button", { name: "立即同步" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_run");
    await openTarget();
    edit("同步分支", "another");
    await tab("同步列表");
    expect(screen.getByRole("button", { name: "立即同步" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "配置" }));
    expect(await screen.findByRole("dialog")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "返回" }));
    await openTarget();
    expect(screen.getByLabelText("同步分支")).toHaveValue("another");
  });

  it.each([
    { readable: false, writable: false, cleanup_ok: true, code: "GIT_READ", probe_ref: null },
    { readable: true, writable: false, cleanup_ok: true, code: "GIT_WRITE", probe_ref: null },
    { readable: true, writable: true, cleanup_ok: false, code: "GIT_CLEANUP", probe_ref: "refs/heads/ai-config-manager-probe/test-123" },
    { readable: false, writable: false, cleanup_ok: true, code: "private-command-output", probe_ref: null },
  ])("shows permission and cleanup stages with fixed code $code", async (stages) => {
    await openSync();
    await openTarget();
    const result: RepositoryProbeResult = { ok: false, latency_ms: 42, ...stages };
    ipc.invoke.mockResolvedValueOnce({ ...result, message: "private-command-output" });
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    const panel = screen.getByRole("status", { name: "测试仓库读写" });
    expect(panel).toHaveTextContent(`读取${stages.readable ? "通过" : "未完成"}`);
    expect(panel).toHaveTextContent(`写入${stages.writable ? "通过" : "未完成"}`);
    expect(panel).toHaveTextContent(`清理${stages.cleanup_ok ? "通过" : "未完成"}`);
    expect(panel).toHaveTextContent(stages.code.startsWith("GIT_") ? stages.code : "UNKNOWN");
    expect(document.body.textContent).not.toContain("private-command-output");
    if (stages.probe_ref) {
      expect(panel).toHaveTextContent("请手动删除");
      expect(panel).toHaveTextContent(stages.probe_ref);
    }
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_save_settings", expect.anything());
  });

  it("invalidates tests on repository/provider changes and external changes while retaining all target drafts", async () => {
    await openSync();
    await openTarget();
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    edit("GitHub 仓库", "alice/draft");
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    await userEvent.selectOptions(screen.getByLabelText("仓库类型"), "git");
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
    edit("Git 仓库", "git@git.example:configs.git");
    edit("同步分支", "draft-branch");
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    sync = syncView({ repo: "external/configs", provider: "github", git_branch: "external" });
    await act(async () => { ipc.events.get("sync-changed")?.(); });
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("仓库类型")).toHaveValue("git");
    expect(screen.getByLabelText("Git 仓库")).toHaveValue("git@git.example:configs.git");
    expect(screen.getByLabelText("同步分支")).toHaveValue("draft-branch");
  });

  it("ignores a successful test response after an external baseline change", async () => {
    await openSync();
    await openTarget();
    let finish!: (value: RepositoryProbeResult) => void;
    ipc.invoke.mockImplementationOnce(() => new Promise<RepositoryProbeResult>((resolve) => { finish = resolve; }));
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    sync = syncView({ repo: "external/configs" });
    await act(async () => { ipc.events.get("sync-changed")?.(); });
    await act(async () => { finish({ ok: true, readable: true, writable: true, cleanup_ok: true, code: "OK", latency_ms: 10, probe_ref: null }); });
    expect(screen.queryByRole("status", { name: "测试仓库读写" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("external/configs");
  });

  it("shows fixed protocol failures without leaking error text or clearing the draft", async () => {
    await openSync();
    await openTarget();
    edit("GitHub 仓库", "alice/draft");
    edit("GitHub Token", "synthetic-draft-token");
    ipc.invoke.mockRejectedValueOnce({ code: "GIT_UNAVAILABLE", message: "private-command-output" });
    await userEvent.click(screen.getByRole("button", { name: "测试仓库读写" }));
    expect(screen.getByRole("status", { name: "测试仓库读写" })).toHaveTextContent("GIT_UNAVAILABLE");
    expect(screen.getByRole("status", { name: "测试仓库读写" })).toHaveTextContent("清理未完成");
    expect(document.body.textContent).not.toContain("private-command-output");
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/draft");
    expect(screen.getByLabelText("GitHub Token")).toHaveValue("synthetic-draft-token");
  });
});

const registration = {
  app_id: "my-editor", app_name: "My Editor", config_id: "preferences", label: "Editor preferences",
  sync_interface: { socket_path: "/home/test/editor/sync.sock", path: "/config" }, enabled: true,
};
function applicationItem(): SyncView["items"][number] {
  return {
    id: "application-editor", label: registration.label, kind: "application", path: null, enabled: true,
    application: { app_id: registration.app_id, app_name: registration.app_name, config_id: registration.config_id, sync_interface: registration.sync_interface },
  };
}
async function tab(name: string) {
  if (["应用注册", "同步记录", "同步列表", "Applications", "Sync records", "Sync list"].includes(name)) {
    await userEvent.click(screen.getByRole("button", { name }));
    return;
  }
  if (!screen.queryByRole("tab", { name })) {
    await userEvent.click(screen.getByRole("button", { name: "设置" }));
  }
  await userEvent.click(screen.getByRole("tab", { name }));
}
function edit(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
}
function fillApplication() {
  for (const [label, value] of [
    ["应用 ID", registration.app_id], ["应用名称", registration.app_name],
    ["配置 ID", registration.config_id], ["配置名称", registration.label],
    ["同步接口 Socket 路径", registration.sync_interface.socket_path],
    ["同步接口请求路径", registration.sync_interface.path],
  ]) edit(label, value);
}
async function openApplicationRegistration() {
  await userEvent.click(screen.getByRole("button", { name: "注册应用" }));
  return screen.getByRole("dialog", { name: "注册应用" });
}
async function closeApplicationRegistration() {
  const dialog = screen.getByRole("dialog", { name: "注册应用" });
  await userEvent.click(within(dialog).getByRole("button", { name: "取消" }));
}
function record(index: number): SyncRecord {
  return {
    id: `record-${index}`, started_at: new Date(Date.UTC(2026, 10, 12, 0, 105 - index)).toISOString(),
    completed_at: new Date(Date.UTC(2026, 10, 12, 0, 105 - index, 30)).toISOString(),
    trigger: "scheduled", state: "success", repo: "alice/configs", item_count: 7,
    uploaded_count: 4, downloaded_count: 2, conflict_count: 1, error_code: null, remote_sha: "digest",
  };
}

describe("application registration, sync records, and automation", () => {
  it("controls automatic participation separately and scopes manual sync to one application", async () => {
    sync.items.push(applicationItem(), { ...applicationItem(), id: "other-editor",
      application: { ...applicationItem().application!, app_id: "other", app_name: "Other" } });
    await openSync();
    await tab("同步列表");
    const auto = screen.getByRole("checkbox", { name: "自动同步: My Editor · Editor preferences" });
    expect(auto).toBeChecked();
    await userEvent.click(auto);
    expect(ipc.invoke).toHaveBeenCalledWith("sync_set_application_auto_sync", { appId: "my-editor", enabled: false });
    expect(auto).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: "自动同步: Other · Editor preferences" })).toBeChecked();
    const manual = screen.getByRole("button", { name: "立即同步: My Editor · Editor preferences" });
    expect(manual).toBeEnabled();
    await userEvent.click(manual);
    expect(ipc.invoke).toHaveBeenCalledWith("sync_run_application", { appId: "my-editor" });
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_run");
    await userEvent.click(screen.getByRole("checkbox", { name: "参与同步: My Editor · Editor preferences" }));
    expect(manual).toBeDisabled();
  });

  it("shows registered applications first and opens a focused dialog only on request, retaining cancelled drafts", async () => {
    sync.items.push({
      id: "registered-editor", label: registration.label, kind: "application", path: null, enabled: true,
      application: { app_id: registration.app_id, app_name: registration.app_name, config_id: registration.config_id, sync_interface: registration.sync_interface },
    });
    await openSync();
    await tab("应用注册");
    expect(screen.getByText("My Editor")).toBeVisible();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("应用 ID")).not.toBeInTheDocument();
    const trigger = screen.getByRole("button", { name: "注册应用" });
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveFocus();
    edit("应用 ID", "pending-editor");
    await closeApplicationRegistration();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    const dialog = await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("pending-editor");
    fireEvent(dialog, new Event("cancel", { cancelable: true }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_register_application", expect.anything());
  });

  it("keeps a pending registration modal open and prevents closing or submitting it twice", async () => {
    await openSync();
    await tab("应用注册");
    const dialog = await openApplicationRegistration();
    fillApplication();
    let finish!: (value: SyncView) => void;
    ipc.invoke.mockImplementationOnce(() => new Promise<SyncView>((resolve) => { finish = resolve; }));
    await userEvent.click(within(dialog).getByRole("button", { name: "注册应用配置" }));
    expect(within(dialog).getByRole("button", { name: "正在保存…" })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "取消" })).toBeDisabled();
    fireEvent(dialog, new Event("cancel", { cancelable: true }));
    fireEvent.submit(dialog.querySelector("form")!);
    expect(dialog).toBeVisible();
    expect(ipc.invoke.mock.calls.filter(([command]) => command === "sync_register_application")).toHaveLength(1);
    await act(async () => { finish(sync); });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("registers the callback contract, isolates applications from files, and enables/removes entries", async () => {
    await openSync();
    await tab("应用注册");
    expect(screen.queryByLabelText("应用 ID")).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await openApplicationRegistration();
    await userEvent.click(screen.getByRole("button", { name: "注册应用配置" }));
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_register_application", expect.anything());
    expect(screen.getByRole("alert")).toHaveTextContent("请填写应用 ID");
    fillApplication();
    await userEvent.click(screen.getByRole("button", { name: "注册应用配置" }));
    await screen.findByText("应用配置已注册。");
    expect(ipc.invoke).toHaveBeenCalledWith("sync_register_application", { request: registration });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.queryByLabelText("应用 ID")).not.toBeInTheDocument();
    expect(screen.getByText("My Editor")).toBeVisible();
    expect(screen.getByText(registration.sync_interface.socket_path)).toBeVisible();
    expect(screen.getByText("/config", { selector: "code" })).toBeVisible();
    expect(screen.getByText(/配置名称: Editor preferences/)).toBeVisible();
    await userEvent.click(screen.getByRole("checkbox", { name: "参与同步: My Editor · Editor preferences" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_set_item_enabled", { id: "application-editor", enabled: false });
    expect(screen.getByRole("checkbox", { name: /参与同步: My Editor/ })).not.toBeChecked();
    await tab("同步列表");
    expect(screen.getByText("My Editor")).toBeVisible();
    await tab("应用注册");
    await userEvent.click(screen.getByRole("button", { name: "移除: My Editor · Editor preferences" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_remove_item", { id: "application-editor" });
    expect(screen.queryByText("My Editor")).not.toBeInTheDocument();
  });

  it("keeps registration drafts on validation and IPC failure, including across tabs", async () => {
    await openSync();
    await tab("应用注册");
    await openApplicationRegistration();
    fillApplication();
    edit("同步接口请求路径", "https://example.invalid/config");
    await userEvent.click(screen.getByRole("button", { name: "注册应用配置" }));
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_register_application", expect.anything());
    edit("同步接口请求路径", "/config");
    ipc.invoke.mockRejectedValueOnce({ code: "SYNC_INTERFACE", message: "private-callback-response" });
    await userEvent.click(screen.getByRole("button", { name: "注册应用配置" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("应用注册信息无效");
    expect(screen.queryByText("private-callback-response")).not.toBeInTheDocument();
    await closeApplicationRegistration();
    await tab("关于");
    await tab("应用注册");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("my-editor");
    expect(screen.getByLabelText("同步接口 Socket 路径")).toHaveValue(registration.sync_interface.socket_path);
  });

  it("loads only the newest 100 records and displays result counts and fixed error codes without contents", async () => {
    sync = syncView({ records: Array.from({ length: 105 }, (_, index) => record(index)) });
    sync.records![0] = { ...record(0), state: "error", trigger: "agent", error_code: "SYNC_CALLBACK" };
    sync.records![1] = { ...record(1), state: "error", trigger: "manual", error_code: "private-config-content" };
    sync.records![2] = { ...record(2), state: "error", error_code: "GIT_TIMEOUT" };
    Object.assign(sync.records![0], { content: "private-config-content", token: "private-token" });
    await openSync();
    await tab("同步记录");
    await waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith("sync_history"));
    const articles = screen.getAllByRole("article");
    expect(articles).toHaveLength(100);
    expect(articles[0]).toHaveTextContent("结果: 失败");
    expect(articles[0]).toHaveTextContent("本地代理");
    expect(articles[0]).toHaveTextContent("SYNC_CALLBACK");
    expect(articles[0]).toHaveTextContent("同步项目7");
    expect(articles[0]).toHaveTextContent("上传4");
    expect(articles[0]).toHaveTextContent("下载2");
    expect(articles[0]).toHaveTextContent("冲突1");
    expect(articles[1]).toHaveTextContent("未知错误");
    expect(articles[2]).toHaveTextContent("定时");
    expect(articles[2]).toHaveTextContent("GIT_TIMEOUT");
    expect(screen.queryByText(record(100).started_at)).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("private-config-content");
    expect(document.body.textContent).not.toContain("private-token");
    sync = { ...sync, records: [] };
    await userEvent.click(screen.getByRole("button", { name: "刷新同步记录" }));
    expect(await screen.findByText("暂无同步记录")).toBeVisible();
  });

  it.each([
    ["同步间隔（分钟）", "0"], ["同步间隔（分钟）", "1441"],
    ["同步间隔（分钟）", "1.5"], ["同步间隔（分钟）", ""],
    ["历史保留天数", "-1"], ["历史保留天数", "3651"],
    ["历史保留天数", "1.5"], ["历史保留天数", ""],
  ])("rejects invalid policy %s=%s without saving", async (label, value) => {
    await openSync();
    await tab("自动化");
    edit(label, value);
    await userEvent.click(screen.getByRole("button", { name: "保存自动化设置" }));
    expect(screen.getByRole("alert")).toHaveTextContent("同步间隔须为 1–1440 的整数");
    expect(ipc.invoke).not.toHaveBeenCalledWith("sync_save_policy", expect.anything());
    await tab("关于");
    await tab("自动化");
    expect((screen.getByLabelText(label) as HTMLInputElement).value).toBe(value);
  });

  it.each([[1, 0], [1440, 3650]])("saves policy boundaries interval=%s retention=%s and shows the next deadline", async (interval, retention) => {
    await openSync();
    await tab("自动化");
    expect(screen.getByText("尚未安排")).toBeVisible();
    await userEvent.click(screen.getByRole("switch", { name: "自动同步" }));
    edit("同步间隔（分钟）", String(interval));
    edit("历史保留天数", String(retention));
    await userEvent.click(screen.getByRole("button", { name: "保存自动化设置" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_save_policy", { policy: {
      auto_sync_enabled: true, sync_interval_minutes: interval, history_retention_days: retention,
    } });
    expect(await screen.findByText("自动化设置已保存。")).toBeInTheDocument();
    expect(screen.getByText("2026-11-12T12:30:00Z")).toBeVisible();
    expect(within(screen.getByRole("tabpanel", { name: "自动化" })).queryByText("未保存")).toBeNull();
  });

  it("retains all drafts across background changes and tabs, then discards only after confirmed page navigation", async () => {
    await openSync();
    await tab("自动化");
    edit("同步间隔（分钟）", "20");
    await tab("应用注册");
    await openApplicationRegistration();
    fillApplication();
    sync = { ...sync, policy: { auto_sync_enabled: true, sync_interval_minutes: 60, history_retention_days: 90 }, next_sync_at: "2026-11-12T13:00:00Z" };
    await act(async () => { ipc.events.get("sync-changed")?.(); });
    expect(screen.getByLabelText("应用 ID")).toHaveValue("my-editor");
    await closeApplicationRegistration();
    await tab("自动化");
    expect(screen.getByLabelText("同步间隔（分钟）")).toHaveValue(20);
    expect(screen.getByLabelText("历史保留天数")).toHaveValue(30);
    expect(screen.getByRole("switch", { name: "自动同步" })).not.toBeChecked();
    expect(screen.getByText("2026-11-12T13:00:00Z")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "历史记录" }));
    await userEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.getByLabelText("同步间隔（分钟）")).toHaveValue(20);
    await userEvent.click(screen.getByRole("button", { name: "历史记录" }));
    await userEvent.click(screen.getByRole("button", { name: "继续" }));
    await userEvent.click(screen.getByRole("button", { name: "设置" }));
    expect(screen.getByLabelText("同步间隔（分钟）")).toHaveValue(60);
    expect(screen.getByLabelText("历史保留天数")).toHaveValue(90);
    await tab("应用注册");
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("");
  });

  it("keeps other panels' drafts when saving general, GitHub, policy, or application settings", async () => {
    await openSync();
    await openTarget();
    edit("GitHub 仓库", "alice/new-target");
    await tab("应用注册");
    await openApplicationRegistration();
    fillApplication();
    await closeApplicationRegistration();
    await tab("自动化");
    edit("同步间隔（分钟）", "20");
    await tab("同步记录");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await tab("常规");
    await userEvent.click(screen.getByRole("switch", { name: /静默启动/ }));
    await tab("同步列表");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("文件路径")).not.toBeInTheDocument();
    await tab("常规");
    expect(screen.getByRole("switch", { name: /静默启动/ })).not.toBeChecked();
    await userEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await openTarget();
    expect(screen.getByLabelText("GitHub 仓库")).toHaveValue("alice/new-target");
    await userEvent.click(screen.getByRole("button", { name: "保存同步配置" }));
    await tab("自动化");
    expect(screen.getByLabelText("同步间隔（分钟）")).toHaveValue(20);
    await userEvent.click(screen.getByRole("button", { name: "保存自动化设置" }));
    await tab("应用注册");
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("my-editor");
    await closeApplicationRegistration();
    await tab("自动化");
    edit("历史保留天数", "0");
    await tab("应用注册");
    await openApplicationRegistration();
    await userEvent.click(screen.getByRole("button", { name: "注册应用配置" }));
    await screen.findByText("应用配置已注册。");
    await tab("自动化");
    expect(screen.getByLabelText("历史保留天数")).toHaveValue(0);
    expect(screen.getByText("未保存")).toBeVisible();
    await tab("同步列表");
    expect(screen.getByText("My Editor")).toBeVisible();
    await userEvent.click(screen.getByRole("checkbox", { name: "参与同步: My Editor · Editor preferences" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_set_item_enabled", { id: "application-editor", enabled: false });
    await tab("自动化");
    expect(screen.getByLabelText("历史保留天数")).toHaveValue(0);
    expect(screen.getByText("未保存")).toBeVisible();
  });

  it("cleans up with the saved policy while preserving pending edits, including after a failed policy save", async () => {
    await openSync();
    await tab("应用注册");
    await openApplicationRegistration();
    fillApplication();
    await closeApplicationRegistration();
    await tab("自动化");
    edit("历史保留天数", "0");
    ipc.invoke.mockRejectedValueOnce({ code: "SYNC_POLICY", message: "private-policy-value" });
    await userEvent.click(screen.getByRole("button", { name: "保存自动化设置" }));
    expect(screen.getByRole("alert")).toHaveTextContent("同步间隔须为");
    expect(screen.getByLabelText("历史保留天数")).toHaveValue(0);
    expect(sync.policy?.history_retention_days).toBe(30);
    await userEvent.click(screen.getByRole("button", { name: "立即清理过期记录" }));
    expect(ipc.invoke).toHaveBeenCalledWith("sync_cleanup");
    expect(await screen.findByText("过期记录已清理。")).toBeInTheDocument();
    expect(screen.getByLabelText("历史保留天数")).toHaveValue(0);
    await tab("应用注册");
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("my-editor");
  });

  it("protects both forms on window close and clears them when close is confirmed", async () => {
    await openSync();
    await tab("自动化");
    edit("同步间隔（分钟）", "20");
    await tab("应用注册");
    await openApplicationRegistration();
    fillApplication();
    await closeApplicationRegistration();
    const unload = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(unload);
    expect(unload.defaultPrevented).toBe(true);
    await userEvent.click(screen.getByRole("button", { name: "关闭窗口" }));
    await userEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(ipc.invoke).not.toHaveBeenCalledWith("window_control", { action: "close" });
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("my-editor");
    await closeApplicationRegistration();
    await userEvent.click(screen.getByRole("button", { name: "关闭窗口" }));
    await userEvent.click(screen.getByRole("button", { name: "继续" }));
    expect(ipc.invoke).toHaveBeenCalledWith("window_control", { action: "close" });
    await openApplicationRegistration();
    expect(screen.getByLabelText("应用 ID")).toHaveValue("");
    await closeApplicationRegistration();
    await tab("自动化");
    expect(screen.getByLabelText("同步间隔（分钟）")).toHaveValue(5);
  });

  it("navigates four translated settings tabs and three sidebar sync pages with old fixtures", async () => {
    sync = syncView({ policy: undefined, records: undefined, next_sync_at: undefined });
    settings.language = "en";
    render(<App />);
    await screen.findByDisplayValue(config.base_url);
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    const names = ["General", "Sync target", "Automation", "About"];
    expect(screen.getAllByRole("tab").map((element) => element.textContent)).toEqual(names);
    screen.getByRole("tab", { name: "General" }).focus();
    for (const name of names.slice(1)) {
      await userEvent.keyboard("{ArrowRight}");
      expect(screen.getByRole("tab", { name })).toHaveFocus();
      expect(screen.getByRole("tab", { name })).toHaveAttribute("aria-selected", "true");
      expect(screen.getByRole("tabpanel", { name })).toBeVisible();
    }
    await userEvent.keyboard("{Home}");
    expect(screen.getByRole("tab", { name: "General" })).toHaveFocus();
    await userEvent.keyboard("{ArrowLeft}");
    expect(screen.getByRole("tab", { name: "About" })).toHaveFocus();
    await userEvent.keyboard("{Home}{End}");
    expect(screen.getByRole("tab", { name: "About" })).toHaveFocus();
    await tab("Automation");
    expect(screen.getByLabelText("Sync interval (minutes)")).toHaveValue(10);
    expect(screen.getByLabelText("History retention (days)")).toHaveValue(30);
    expect(screen.getByText("Not scheduled")).toBeVisible();
    for (const name of ["Sync list", "Applications", "Sync records"]) {
      await tab(name);
      expect(screen.getByRole("button", { name })).toHaveAttribute("aria-current", "page");
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(name);
    }
    await tab("Applications");
    await userEvent.click(screen.getByRole("button", { name: "Register application" }));
    expect(screen.getByRole("dialog", { name: "Register application" })).toBeVisible();
    expect(screen.getByLabelText("Application ID")).toHaveFocus();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
