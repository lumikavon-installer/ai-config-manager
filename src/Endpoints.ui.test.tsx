// Named model endpoints in the config editor, using synthetic IPC only.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import type { ConfigView } from "./types";

const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(() => true),
  listen: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ipc);
vi.mock("@tauri-apps/api/event", () => ipc);

const chat: ConfigView = {
  exists: true,
  base_url: "https://app.example/v1",
  model: "chat-model",
  key_present: true,
  encrypted: true,
  revision: "file-rev",
  endpoint: "openai",
  configured: true,
};
const embedding: ConfigView = {
  exists: true,
  base_url: "",
  model: "",
  key_present: false,
  encrypted: false,
  revision: "file-rev",
  endpoint: "embedding",
  configured: false,
};
const preferences = {
  silent_start: false,
  close_to_tray: true,
  language: "zh",
  autostart: false,
  autostart_known: true,
  version: "0.1.0",
  platform: "windows",
  tray_available: true,
};

beforeEach(() => {
  ipc.invoke.mockReset();
  ipc.listen.mockReset();
  ipc.listen.mockImplementation(async () => () => {});
  ipc.invoke.mockImplementation(
    async (command: string, args?: { kind?: string; request?: object }) => {
      if (command === "settings_read") return preferences;
      if (command === "config_read") return { ...chat };
      if (command === "endpoint_read") return { ...embedding };
      if (command === "endpoint_save") {
        const request = args!.request as {
          base_url: string;
          model: string;
        };
        return {
          ...embedding,
          configured: true,
          base_url: request.base_url,
          model: request.model,
          revision: "file-rev-2",
        };
      }
      if (command === "history_list") return [];
      throw new Error(`unexpected IPC command: ${command}`);
    },
  );
});

describe("named model endpoint editor", () => {
  it("loads local defaults and saves only the selected endpoint", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("tab", { name: "向量模型" }));
    const base = screen.getByLabelText("API 地址") as HTMLInputElement;
    await waitFor(() => expect(base.value).toBe("http://127.0.0.1:8848/v1"));
    const model = screen.getByLabelText("模型名称") as HTMLInputElement;
    expect(model.value).toBe("");
    await user.type(model, "embedding-model");
    await user.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() =>
      expect(ipc.invoke.mock.calls.some(([name]) => name === "endpoint_save")).toBe(
        true,
      ),
    );
    const save = ipc.invoke.mock.calls.find(([name]) => name === "endpoint_save");
    expect(save?.[1]).toEqual({
      kind: "embedding",
      request: {
        base_url: "http://127.0.0.1:8848/v1",
        model: "embedding-model",
        api_key: null,
        encrypt: true,
        expected_revision: "file-rev",
      },
    });
    cleanup();
  });
});
