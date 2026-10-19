import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defaultAiSettings } from "../../domain/ai";

const mocks = vi.hoisted(() => ({ native: true, invoke: vi.fn() }));

vi.mock("../../infrastructure/platform", () => ({
  platform: {
    get isNative() { return mocks.native; },
    invoke: mocks.invoke,
  },
}));

import { useAiStore } from "./ai";

describe("AI account store", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.native = true;
    setActivePinia(createPinia());
  });

  it("clears cached model choices when account status has no plan usage grant", async () => {
    const store = useAiStore();
    store.models = [{ slug: "stale-model", displayName: "Stale" }];
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "get_chatgpt_ai_settings") return { ...defaultAiSettings };
      if (command === "get_chatgpt_connection_status") return { connected: true, planUsageAuthorized: false };
      throw new Error(`unexpected command ${command}`);
    });

    await store.load();

    expect(store.connection).toEqual({ connected: true, planUsageAuthorized: false });
    expect(store.models).toEqual([]);
  });

  it("enables the AI features automatically for a connected plan-usage account", async () => {
    const store = useAiStore();
    mocks.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "get_chatgpt_ai_settings") return { ...defaultAiSettings };
      if (command === "get_chatgpt_connection_status") return { connected: true, planUsageAuthorized: true };
      if (command === "save_chatgpt_ai_settings") return {
        ...(args?.settings as object),
        privacyNoticeVersion: 2,
        privacyNoticeAcceptedAt: "2026-10-10T00:00:00Z",
      };
      throw new Error(`unexpected command ${command}`);
    });

    await store.load();

    expect(mocks.invoke).toHaveBeenCalledWith("save_chatgpt_ai_settings", {
      settings: expect.objectContaining({ enabled: true, allowAiQa: true, allowAiDailyQuote: true }),
      acceptPrivacyNotice: true,
    });
    expect(store.settings).toMatchObject({ enabled: true, allowAiQa: true, allowAiDailyQuote: true });
  });

  it("clears a selected model removed from the live account catalog", async () => {
    const store = useAiStore();
    store.settings = { ...defaultAiSettings, selectedModelSlug: "removed-model" };
    store.connection = { connected: true, planUsageAuthorized: true };
    mocks.invoke.mockResolvedValue([{ slug: "current-model", displayName: "Current" }]);

    await store.refreshModels();

    expect(store.models).toEqual([{ slug: "current-model", displayName: "Current" }]);
    expect(store.settings.selectedModelSlug).toBeNull();
  });

  it.each([
    [false, "disconnect_chatgpt"],
    [true, "forget_chatgpt_registration"],
  ])("disables AI and clears the model after account removal (forget=%s)", async (forget, expectedCommand) => {
    const store = useAiStore();
    store.settings = {
      ...defaultAiSettings,
      enabled: true,
      selectedModelSlug: "gpt-example",
      allowAiQa: true,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    };
    store.connection = { connected: true, planUsageAuthorized: true };
    store.models = [{ slug: "gpt-example", displayName: "Example" }];
    const disconnectResult = {
      localCredentialsRemoved: true,
      remoteRevocationConfirmed: true,
      message: "已断开。",
    };
    mocks.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === expectedCommand) return disconnectResult;
      if (command === "save_chatgpt_ai_settings") return args?.settings;
      throw new Error(`unexpected command ${command}`);
    });

    await store.disconnect(forget);

    expect(mocks.invoke).toHaveBeenNthCalledWith(1, expectedCommand);
    expect(mocks.invoke).toHaveBeenNthCalledWith(2, "save_chatgpt_ai_settings", {
      settings: expect.objectContaining({
        enabled: false,
        selectedModelSlug: null,
        allowAiQa: false,
        allowAiDailyQuote: false,
      }),
      acceptPrivacyNotice: false,
    });
    expect(store.connection).toEqual({ connected: false, planUsageAuthorized: false });
    expect(store.models).toEqual([]);
    expect(store.settings.selectedModelSlug).toBeNull();
    expect(store.settings.allowAiQa).toBe(false);
    expect(store.settings.allowAiDailyQuote).toBe(false);
  });
});
