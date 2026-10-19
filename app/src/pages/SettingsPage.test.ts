// @vitest-environment jsdom
import { mount } from "@vue/test-utils";
import { reactive, ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defaultAiSettings } from "../domain/ai";

const mocks = vi.hoisted(() => ({
  native: true,
  preferencesStore: null as unknown,
  aiStore: null as unknown,
  invoke: vi.fn(),
}));

vi.mock("../app/stores/preferences", () => ({ usePreferencesStore: () => mocks.preferencesStore }));
vi.mock("../app/stores/ai", () => ({ useAiStore: () => mocks.aiStore }));
vi.mock("../infrastructure/platform", () => ({
  platform: {
    get isNative() { return mocks.native; },
    invoke: mocks.invoke,
  },
}));

import SettingsPage from "./SettingsPage.vue";

function mountSettings() {
  return mount(SettingsPage);
}

describe("ChatGPT AI settings UI", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.native = true;
    mocks.preferencesStore = reactive({
      value: { theme: "paper", fontSizePx: 18, lineHeight: 1.85, contentWidthPx: 720 },
      error: "",
      update: vi.fn(),
    });
    mocks.aiStore = reactive({
      settings: { ...defaultAiSettings },
      connection: { connected: false, planUsageAuthorized: false },
      models: [],
      loaded: ref(true),
      connecting: false,
      modelLoading: false,
      modelError: "",
      error: "",
      save: vi.fn(async (settings: unknown) => settings),
      connect: vi.fn(),
      cancelSignIn: vi.fn(),
      refreshModels: vi.fn(async () => []),
      disconnect: vi.fn(),
    });
  });

  it("removes AI feature switches and disclosures after account connection", () => {
    mocks.aiStore = reactive({
      ...mocks.aiStore as object,
      settings: { ...defaultAiSettings, enabled: true, allowAiQa: true, allowAiDailyQuote: true },
      connection: { connected: true, planUsageAuthorized: true },
      models: [{ slug: "gpt-live-model", displayName: "Live model" }],
    });
    const wrapper = mountSettings();

    expect(wrapper.get("h1").text()).toBe("设置");
    expect(wrapper.text()).not.toContain("偏好设置");
    expect(wrapper.find(".settings-aside").exists()).toBe(false);

    for (const removedCopy of ["启用AI功能", "允许阅读AI问答", "允许AI挑选今日摘录", "数据与用量说明", "保存AI设置与许可", "在线书籍发现"]) {
      expect(wrapper.text()).not.toContain(removedCopy);
    }
    for (const removedElement of [".ai-privacy-lead", ".ai-privacy-notice", ".ai-enabled-row", ".ai-consent-option", ".ai-actions"]) {
      expect(wrapper.find(removedElement).exists()).toBe(false);
    }

    const actions = wrapper.findAll(".ai-connection-actions button");
    expect(actions.map((button) => button.text())).toEqual(["重新连接", "退出登录"]);
    for (const button of actions) {
      expect(button.classes()).toContain("button");
      expect(button.classes()).toContain("button-secondary");
    }
  });

  it("offers account connection and reports an identity without plan usage", () => {
    const disconnected = mountSettings();
    expect(disconnected.find(".ai-connection-actions").text()).toContain("Continue with ChatGPT");
    expect(disconnected.find(".ai-model-row").exists()).toBe(false);

    mocks.aiStore = reactive({
      ...mocks.aiStore as object,
      connection: { connected: true, planUsageAuthorized: false },
    });
    const identityOnly = mountSettings();
    expect(identityOnly.text()).toContain("OAuth未授予`chatgpt.tokens.use.direct`");
    expect(identityOnly.find(".ai-model-row").exists()).toBe(false);
  });

  it("requires confirmation before saving the selected live model", async () => {
    const save = vi.fn(async (settings: unknown) => settings);
    mocks.aiStore = reactive({
      ...mocks.aiStore as object,
      settings: { ...defaultAiSettings, enabled: true, selectedModelSlug: "gpt-one" },
      connection: { connected: true, planUsageAuthorized: true },
      models: [
        { slug: "gpt-one", displayName: "Model One" },
        { slug: "gpt-two", displayName: "Model Two" },
      ],
      save,
    });
    const wrapper = mountSettings();

    await wrapper.get("select").setValue("gpt-two");

    expect(save).not.toHaveBeenCalled();
    const confirm = wrapper.get(".ai-confirm-model");
    expect(confirm.text()).toContain("确认选择");
    await confirm.trigger("click");
    expect(save).toHaveBeenCalledWith(expect.objectContaining({ selectedModelSlug: "gpt-two" }));
    expect(wrapper.get("select").text()).toContain("Model Two");
    expect(wrapper.get(".ai-notice").text()).toContain("Model Two");
    expect(wrapper.get(".ai-notice").text()).toContain("AI将使用此模型");
    expect(wrapper.get(".ai-notice").attributes("role")).toBe("status");
  });

  it("confirms the current model too and shows feedback", async () => {
    const save = vi.fn(async (settings: unknown) => settings);
    mocks.aiStore = reactive({
      ...mocks.aiStore as object,
      settings: { ...defaultAiSettings, enabled: true, selectedModelSlug: "gpt-one" },
      connection: { connected: true, planUsageAuthorized: true },
      models: [{ slug: "gpt-one", displayName: "Model One" }],
      save,
    });
    const wrapper = mountSettings();
    const confirm = wrapper.get(".ai-confirm-model");

    expect(confirm.attributes("disabled")).toBeUndefined();
    await confirm.trigger("click");

    expect(save).toHaveBeenCalledWith(expect.objectContaining({ selectedModelSlug: "gpt-one" }));
    expect(wrapper.get(".ai-notice").text()).toContain("已确认使用Model One");
    expect(wrapper.get(".ai-notice").attributes("role")).toBe("status");
  });

  it("keeps account connection unavailable in browser preview", () => {
    mocks.native = false;
    const wrapper = mountSettings();
    expect(wrapper.get(".ai-connection-actions button").attributes("disabled")).toBeDefined();
    expect(wrapper.find(".ai-model-row").exists()).toBe(false);
    expect(mocks.invoke).not.toHaveBeenCalled();
  });
});
