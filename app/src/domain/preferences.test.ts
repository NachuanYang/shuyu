import { describe, expect, it } from "vitest";
import { defaultPreferences, normalizePreferences } from "./preferences";

describe("reader preferences", () => {
  it("starts private with the dark theme by default", () => {
    expect(defaultPreferences).toMatchObject({
      theme: "dark",
      fontSizePx: 18,
      lineHeight: 1.85,
      contentWidthPx: 720,
      enableOnlineDiscovery: false,
    });
  });

  it("falls back from corrupted stored values and clamps supported ranges", () => {
    expect(
      normalizePreferences({
        theme: "neon",
        fontSizePx: 99,
        lineHeight: 0.4,
        contentWidthPx: 1200,
      }),
    ).toMatchObject({
      theme: "dark",
      fontSizePx: 28,
      lineHeight: 1.4,
      contentWidthPx: 860,
      enableOnlineDiscovery: false,
    });
  });

  it("preserves the explicit online discovery preference value", () => {
    expect(normalizePreferences({ enableOnlineDiscovery: true })).toMatchObject({ enableOnlineDiscovery: true });
    expect(normalizePreferences({ enableOnlineDiscovery: false })).toMatchObject({ enableOnlineDiscovery: false });
  });
});
