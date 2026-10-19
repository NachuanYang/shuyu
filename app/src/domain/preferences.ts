export type ReaderPreferences = {
  theme: "paper" | "light" | "dark";
  fontFamily: "system-serif" | "system-sans";
  fontSizePx: number;
  lineHeight: number;
  contentWidthPx: number;
  paragraphSpacingEm: number;
  enableOnlineDiscovery: boolean;
};

export const defaultPreferences: ReaderPreferences = Object.freeze({
  theme: "dark",
  fontFamily: "system-serif",
  fontSizePx: 18,
  lineHeight: 1.85,
  contentWidthPx: 720,
  paragraphSpacingEm: 0.9,
  enableOnlineDiscovery: false,
});

function boundedNumber(value: unknown, fallback: number, minimum: number, maximum: number, decimals = 0): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  const bounded = Math.min(maximum, Math.max(minimum, value));
  const factor = 10 ** decimals;
  return Math.round(bounded * factor) / factor;
}

export function normalizePreferences(value: unknown): ReaderPreferences {
  const input = value && typeof value === "object" ? (value as Record<string, unknown>) : {};
  return {
    theme: input.theme === "paper" || input.theme === "light" || input.theme === "dark" ? input.theme : "dark",
    fontFamily: input.fontFamily === "system-sans" ? "system-sans" : "system-serif",
    fontSizePx: boundedNumber(input.fontSizePx, defaultPreferences.fontSizePx, 14, 28),
    lineHeight: boundedNumber(input.lineHeight, defaultPreferences.lineHeight, 1.4, 2.2, 2),
    contentWidthPx: boundedNumber(input.contentWidthPx, defaultPreferences.contentWidthPx, 540, 860),
    paragraphSpacingEm: boundedNumber(input.paragraphSpacingEm, defaultPreferences.paragraphSpacingEm, 0.4, 1.6, 1),
    enableOnlineDiscovery: input.enableOnlineDiscovery === true,
  };
}
