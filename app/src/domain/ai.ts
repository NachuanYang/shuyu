export const AI_PRIVACY_NOTICE_VERSION = 2;

export interface AiSettings {
  enabled: boolean;
  selectedModelSlug: string | null;
  allowAiQa: boolean;
  allowAiDailyQuote: boolean;
  privacyNoticeVersion: number;
  privacyNoticeAcceptedAt: string | null;
}

export interface ChatGptConnectionStatus {
  connected: boolean;
  planUsageAuthorized: boolean;
}

export interface ChatGptModel {
  slug: string;
  displayName: string;
}

export type ChatGptConnectionState = "disconnected" | "identity-only" | "plan-usage-ready";

export const defaultAiSettings: Readonly<AiSettings> = Object.freeze({
  enabled: false,
  selectedModelSlug: null,
  allowAiQa: false,
  allowAiDailyQuote: false,
  privacyNoticeVersion: 0,
  privacyNoticeAcceptedAt: null,
});

const MAX_SELECTION_LENGTH = 16_384;
const MAX_QUESTION_LENGTH = 1_000;

export function normalizeAiSettings(value: unknown): AiSettings {
  const input = value && typeof value === "object" ? (value as Record<string, unknown>) : {};
  const privacyNoticeVersion = input.privacyNoticeVersion === AI_PRIVACY_NOTICE_VERSION
    ? AI_PRIVACY_NOTICE_VERSION
    : 0;
  const privacyNoticeAcceptedAt = typeof input.privacyNoticeAcceptedAt === "string"
    && input.privacyNoticeAcceptedAt.length > 0
    && input.privacyNoticeAcceptedAt.length <= 64
    ? input.privacyNoticeAcceptedAt
    : null;
  const hasCurrentConsent = privacyNoticeVersion === AI_PRIVACY_NOTICE_VERSION && !!privacyNoticeAcceptedAt;
  const model = typeof input.selectedModelSlug === "string" ? input.selectedModelSlug.trim() : "";
  const selectedModelSlug = isValidModelSlug(model) ? model : null;
  const allowAiQa = hasCurrentConsent && input.allowAiQa === true;
  return {
    enabled: input.enabled === true,
    selectedModelSlug,
    allowAiQa,
    allowAiDailyQuote: hasCurrentConsent && input.allowAiDailyQuote === true,
    privacyNoticeVersion: hasCurrentConsent ? privacyNoticeVersion : 0,
    privacyNoticeAcceptedAt: hasCurrentConsent ? privacyNoticeAcceptedAt : null,
  };
}

export function isValidModelSlug(value: string): boolean {
  return value.length > 0 && value.length <= 256 && /^[A-Za-z0-9_./-]+$/.test(value);
}

export function getChatGptConnectionState(status: ChatGptConnectionStatus): ChatGptConnectionState {
  if (!status.connected) return "disconnected";
  return status.planUsageAuthorized ? "plan-usage-ready" : "identity-only";
}

export function canUseAiQuestion(
  settings: AiSettings,
  connection: ChatGptConnectionStatus,
  isNative: boolean,
): boolean {
  return isNative && settings.enabled && settings.allowAiQa
    && connection.connected && connection.planUsageAuthorized
    && Boolean(settings.selectedModelSlug);
}

export function canUseAiDailyQuote(
  settings: AiSettings,
  connection: ChatGptConnectionStatus,
  isNative: boolean,
): boolean {
  return isNative && settings.enabled && settings.allowAiDailyQuote
    && connection.connected && connection.planUsageAuthorized
    && Boolean(settings.selectedModelSlug);
}

export function validateSelectedText(value: string): string {
  const selection = value.trim();
  if (!selection) throw new Error("请选择需要解释的原文。");
  if ([...selection].length > MAX_SELECTION_LENGTH) throw new Error("选段过长，请缩短到16,384个字符以内。");
  return selection;
}

export function validateAiQuestion(value: string): string {
  const question = value.trim();
  if ([...question].length > MAX_QUESTION_LENGTH) throw new Error("问题不能超过1,000个字符。");
  return question;
}
