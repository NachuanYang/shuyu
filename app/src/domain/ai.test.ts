import { describe, expect, it } from "vitest";
import {
  canUseAiDailyQuote,
  canUseAiQuestion,
  defaultAiSettings,
  getChatGptConnectionState,
  normalizeAiSettings,
  validateAiQuestion,
  validateSelectedText,
} from "./ai";

describe("AI client settings", () => {
  it("requires renewed consent after the Q&A data scope expands", () => {
    expect(normalizeAiSettings({
      allowAiQa: true,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 1,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    })).toMatchObject({ allowAiQa: false, allowAiDailyQuote: false, privacyNoticeVersion: 0 });
  });

  it("starts with every AI capability disabled and no selected model", () => {
    expect(defaultAiSettings).toEqual({
      enabled: false,
      selectedModelSlug: null,
      allowAiQa: false,
      allowAiDailyQuote: false,
      privacyNoticeVersion: 0,
      privacyNoticeAcceptedAt: null,
    });
  });

  it("normalizes model choice and does not expose secrets from stored data", () => {
    expect(normalizeAiSettings({
      enabled: true,
      selectedModelSlug: " gpt-example ",
      allowAiQa: true,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
      accessToken: "must-not-be-returned",
    })).toEqual({
      enabled: true,
      selectedModelSlug: "gpt-example",
      allowAiQa: true,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    });
  });

  it("disables permissions when privacy consent is absent or stale", () => {
    expect(normalizeAiSettings({
      enabled: true,
      allowAiQa: true,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 0,
      privacyNoticeAcceptedAt: null,
    })).toMatchObject({
      enabled: true,
      allowAiQa: false,
      allowAiDailyQuote: false,
      privacyNoticeVersion: 0,
      privacyNoticeAcceptedAt: null,
    });
  });

  it("rejects an invalid or oversized model slug without inventing a fallback", () => {
    expect(normalizeAiSettings({ selectedModelSlug: "../../private model" }).selectedModelSlug).toBeNull();
    expect(normalizeAiSettings({ selectedModelSlug: "gpt-" + "x".repeat(300) }).selectedModelSlug).toBeNull();
    expect(normalizeAiSettings({}).selectedModelSlug).toBeNull();
  });

  it("does not mark browser preview as connected or configured", () => {
    expect(normalizeAiSettings(null)).toEqual(defaultAiSettings);
    expect(normalizeAiSettings({ connected: true, model: "fake-model" })).toEqual(defaultAiSettings);
  });

  it("keeps independent Q&A and daily quote permissions", () => {
    expect(normalizeAiSettings({
      allowAiQa: true,
      allowAiDailyQuote: false,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    })).toMatchObject({ allowAiQa: true, allowAiDailyQuote: false });
    expect(normalizeAiSettings({
      allowAiQa: false,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    })).toMatchObject({ allowAiQa: false, allowAiDailyQuote: true });
  });

  it("requires native runtime, connection scope, selected model, master enable, and the feature-specific permission", () => {
    const both = normalizeAiSettings({
      enabled: true,
      selectedModelSlug: "gpt-example",
      allowAiQa: true,
      allowAiDailyQuote: true,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    });
    const authorized = { connected: true, planUsageAuthorized: true };
    expect(canUseAiQuestion(both, authorized, true)).toBe(true);
    expect(canUseAiDailyQuote(both, authorized, true)).toBe(true);
    expect(canUseAiQuestion(both, authorized, false)).toBe(false);
    expect(canUseAiQuestion(both, { connected: true, planUsageAuthorized: false }, true)).toBe(false);
    expect(canUseAiDailyQuote({ ...both, allowAiDailyQuote: false }, authorized, true)).toBe(false);
    expect(canUseAiQuestion({ ...both, selectedModelSlug: null }, authorized, true)).toBe(false);
    expect(canUseAiQuestion({ ...both, enabled: false }, authorized, true)).toBe(false);
  });

  it("distinguishes disconnected, identity-only, and plan-usage-ready states", () => {
    expect(getChatGptConnectionState({ connected: false, planUsageAuthorized: false })).toBe("disconnected");
    expect(getChatGptConnectionState({ connected: true, planUsageAuthorized: false })).toBe("identity-only");
    expect(getChatGptConnectionState({ connected: true, planUsageAuthorized: true })).toBe("plan-usage-ready");
  });

  it("requires a valid nonempty timestamp with the current notice version", () => {
    expect(normalizeAiSettings({
      allowAiQa: true,
      privacyNoticeVersion: 1,
      privacyNoticeAcceptedAt: "",
    }).allowAiQa).toBe(false);
    expect(normalizeAiSettings({
      allowAiQa: true,
      privacyNoticeVersion: 99,
      privacyNoticeAcceptedAt: "2026-10-09T12:00:00Z",
    }).allowAiQa).toBe(false);
  });

  it("keeps model slugs as identifiers and displays no fake catalog", () => {
    expect(normalizeAiSettings({ selectedModelSlug: "provider/model-v1" }).selectedModelSlug).toBe("provider/model-v1");
    expect(normalizeAiSettings({ selectedModelSlug: "model name" }).selectedModelSlug).toBeNull();
    expect(normalizeAiSettings({ selectedModelSlug: "" }).selectedModelSlug).toBeNull();
  });

  it("leaves legacy provider data out of the new settings shape", () => {
    expect(normalizeAiSettings({
      provider: "openrouter",
      ollamaEndpoint: "http://127.0.0.1:11434",
      openRouterApiKey: "secret",
      model: "vendor/legacy",
    })).toEqual(defaultAiSettings);
  });

  it("can represent identity connection separately from plan usage permission", () => {
    const connectedWithoutUsage = { connected: true, planUsageAuthorized: false };
    expect(connectedWithoutUsage.connected).toBe(true);
    expect(connectedWithoutUsage.planUsageAuthorized).toBe(false);
  });

  it("does not send browser-only or status fields as AI settings", () => {
    expect(normalizeAiSettings({ connection: { connected: true }, models: ["fake"] })).toEqual(defaultAiSettings);
  });

  it("keeps model selection empty after account removal", () => {
    expect(normalizeAiSettings({ enabled: false, selectedModelSlug: null })).toEqual(defaultAiSettings);
  });

  it("does not normalize unknown provider configuration into a usable model", () => {
    expect(normalizeAiSettings({ provider: "other", model: "remote" })).toMatchObject({
      enabled: false,
      selectedModelSlug: null,
    });
  });
});

describe("selection explanation input", () => {
  it("rejects an empty or overlong selected passage", () => {
    expect(() => validateSelectedText("  ")).toThrow("请选择需要解释的原文");
    expect(() => validateSelectedText("文".repeat(16_385))).toThrow("选段过长");
    expect(validateSelectedText("  原文片段  ")).toBe("原文片段");
  });

  it("bounds the question while allowing an empty question to request an explanation", () => {
    expect(validateAiQuestion("  这句话是什么意思？ ")).toBe("这句话是什么意思？");
    expect(validateAiQuestion("")).toBe("");
    expect(() => validateAiQuestion("问".repeat(1_001))).toThrow("问题不能超过1,000个字符");
  });
});
