// @vitest-environment jsdom
import { mount, flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defaultAiSettings } from "../domain/ai";
import HomePage from "./HomePage.vue";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  native: true,
  settings: null as unknown,
  ai: null as unknown,
}));

vi.mock("../app/stores/preferences", () => ({ usePreferencesStore: () => mocks.settings }));
vi.mock("../app/stores/ai", () => ({ useAiStore: () => mocks.ai }));
vi.mock("../infrastructure/platform", () => ({
  platform: {
    get isNative() { return mocks.native; },
    invoke: mocks.invoke,
  },
}));

const localQuote = {
  localDate: "2026-10-09",
  bookId: "book-fixture",
  bookTitle: "Synthetic fixture",
  author: "Synthetic author",
  chapterId: "chapter-fixture",
  chapterTitle: "Synthetic chapter",
  paragraphId: "paragraph-fixture",
  startIntraUtf16: 0,
  endIntraUtf16: 26,
  textSnapshot: "Synthetic local reading quote.",
  sourceHash: "fixture-hash",
  createdAt: "2026-10-09T00:00:00Z",
  sourceLocalDate: "2026-10-08",
  selectionMethod: "local" as const,
};

describe("HomePage AI request gate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.native = true;
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("does not invoke AI quote selection when AI permissions are off", async () => {
    mocks.settings = { value: { enableOnlineDiscovery: false } };
    mocks.ai = {
      settings: { ...defaultAiSettings },
      connection: { connected: true, planUsageAuthorized: true },
    };
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "list_books") return Promise.resolve([]);
      if (command === "get_daily_quote") return Promise.resolve(localQuote);
      return Promise.resolve(null);
    });

    const wrapper = mount(HomePage, { global: { stubs: { RouterLink: true } } });
    await flushPromises();

    expect(mocks.invoke).toHaveBeenCalledWith("get_daily_quote");
    expect(mocks.invoke).not.toHaveBeenCalledWith("select_ai_daily_quote", expect.anything());
    expect(wrapper.get(".quote-attribution").text()).toBe("——《Synthetic fixture》 · Synthetic author · Synthetic chapter");
    expect(wrapper.text()).not.toContain("AI选取");
    expect(wrapper.text()).not.toContain("选自2026-10-08");
  });

  it("does not automatically refresh the quote and still allows manual selection", async () => {
    vi.useFakeTimers();
    mocks.settings = { value: { enableOnlineDiscovery: false } };
    mocks.ai = {
      settings: {
        ...defaultAiSettings,
        enabled: true,
        selectedModelSlug: "gpt-synthetic",
        allowAiQa: true,
        allowAiDailyQuote: true,
        privacyNoticeVersion: 1,
        privacyNoticeAcceptedAt: "2026-10-10T00:00:00Z",
      },
      connection: { connected: true, planUsageAuthorized: true },
    };
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "list_books") return [];
      if (command === "get_daily_quote") return localQuote;
      if (command === "select_ai_daily_quote") return { quote: localQuote, aiError: null };
      return null;
    });

    const wrapper = mount(HomePage, { global: { stubs: { RouterLink: true } } });
    await flushPromises();
    expect(mocks.invoke).toHaveBeenCalledWith("select_ai_daily_quote", { forceRefresh: false });

    await vi.advanceTimersByTimeAsync(120_000);
    await flushPromises();
    expect(mocks.invoke.mock.calls.filter(([command]) => command === "select_ai_daily_quote")).toHaveLength(1);

    await wrapper.get(".quote-retry-button").trigger("click");
    await flushPromises();
    expect(mocks.invoke).toHaveBeenCalledWith("select_ai_daily_quote", { forceRefresh: true });

    wrapper.unmount();
    const callsAfterUnmount = mocks.invoke.mock.calls.filter(([command]) => command === "select_ai_daily_quote").length;
    await vi.advanceTimersByTimeAsync(120_000);
    await flushPromises();
    expect(mocks.invoke.mock.calls.filter(([command]) => command === "select_ai_daily_quote")).toHaveLength(callsAfterUnmount);
  });

  it("shows today's quote before recent reading in a plain, compact section", async () => {
    mocks.settings = { value: { enableOnlineDiscovery: false } };
    mocks.ai = {
      settings: { ...defaultAiSettings },
      connection: { connected: false, planUsageAuthorized: false },
    };
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "list_books") return [{
        id: "book-home",
        title: "Home fixture",
        author: "Fixture author",
        chapterCount: 1,
        totalUtf16: 20,
        createdAt: "2026-10-10T00:00:00Z",
        lastOpenedAt: "2026-10-10T00:00:00Z",
        progressRatio: 0.2,
      }];
      if (command === "get_daily_quote") return localQuote;
      return null;
    });

    const wrapper = mount(HomePage, { global: { stubs: { RouterLink: true } } });
    await flushPromises();

    const quote = wrapper.get(".home-quote");
    const reading = wrapper.get(".home-reading-section");
    expect(quote.element.compareDocumentPosition(reading.element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(wrapper.find(".home-quote.card-surface").exists()).toBe(false);
    expect(reading.find("h2").exists()).toBe(false);
  });

  it("searches freeform interests and links each result to its Wikipedia article", async () => {
    mocks.settings = { value: {} };
    mocks.ai = {
      settings: { ...defaultAiSettings, enabled: true, selectedModelSlug: "gpt-synthetic", allowAiQa: true, allowAiDailyQuote: true, privacyNoticeVersion: 2, privacyNoticeAcceptedAt: "2026-10-10T00:00:00Z" },
      connection: { connected: true, planUsageAuthorized: true },
    };
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "list_books") return [];
      if (command === "get_daily_quote") return null;
      if (command === "recommend_books") return {
        items: [{ title: "另一种生活", author: "示例作者", reason: "作者相同，主题也相近。" }],
        message: "根据你的兴趣推荐1本书。",
      };
      return null;
    });
    const fetchMock = vi.fn(async (_input: string) => ({
      ok: true,
      json: async () => ({ query: { pages: [{ title: "另一种生活", fullurl: "https://zh.wikipedia.org/wiki/%E5%8F%A6%E4%B8%80%E7%A7%8D%E7%94%9F%E6%B4%BB" }] } }),
    }));
    vi.stubGlobal("fetch", fetchMock);

    const wrapper = mount(HomePage, { global: { stubs: { RouterLink: true } } });
    await flushPromises();
    const input = wrapper.get(".discovery-query-field input");
    expect(input.attributes("maxlength")).toBe("1000");
    await input.setValue("喜欢张爱玲，想看都市关系与女性成长，也接受同主题的其他作者");
    await wrapper.get(".discovery-search").trigger("submit");
    await flushPromises();

    expect(mocks.invoke).toHaveBeenCalledWith("recommend_books", {
      query: "喜欢张爱玲，想看都市关系与女性成长，也接受同主题的其他作者",
    });
    expect(wrapper.text()).toContain("作者相同，主题也相近。");
    expect(wrapper.text()).not.toContain("书架书名、作者、章节数和阅读进度");
    expect(wrapper.text()).not.toContain("不会发送正文、笔记、书签或摘录");
    expect(wrapper.get(".discovery-search button[type='submit']").text()).toBe("查找");
    expect(wrapper.get(".discovery-book-link").attributes("href")).toBe("https://zh.wikipedia.org/wiki/%E5%8F%A6%E4%B8%80%E7%A7%8D%E7%94%9F%E6%B4%BB");
    expect(wrapper.get(".discovery-book-link").attributes("target")).toBe("_blank");
    expect(fetchMock).toHaveBeenCalled();
    const requestUrl = new URL(fetchMock.mock.calls[0]![0]);
    expect(requestUrl.hostname).toBe("zh.wikipedia.org");
    expect(requestUrl.searchParams.get("action")).toBe("query");
    expect(requestUrl.searchParams.get("titles")).toBe("另一种生活");
    expect(requestUrl.searchParams.get("origin")).toBe("*");
    expect(wrapper.text()).not.toContain("引用来源");
    await wrapper.get(".discovery-book-link").trigger("click");
    expect(mocks.invoke).toHaveBeenCalledWith("open_wikipedia_article", {
      url: "https://zh.wikipedia.org/wiki/%E5%8F%A6%E4%B8%80%E7%A7%8D%E7%94%9F%E6%B4%BB",
    });
  });

  it("keeps a recommendation clickable when Wikipedia has no exact article", async () => {
    mocks.settings = { value: {} };
    mocks.ai = {
      settings: { ...defaultAiSettings, enabled: true, selectedModelSlug: "gpt-synthetic", allowAiQa: true, allowAiDailyQuote: true, privacyNoticeVersion: 2, privacyNoticeAcceptedAt: "2026-10-10T00:00:00Z" },
      connection: { connected: true, planUsageAuthorized: true },
    };
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "list_books") return [];
      if (command === "get_daily_quote") return null;
      if (command === "recommend_books") return {
        items: [{ title: "尚无词条的书", author: "示例作者", reason: "与兴趣接近。" }],
        message: "根据你的兴趣推荐1本书。",
      };
      return null;
    });
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      json: async () => ({ query: { pages: [{ pageid: -1, title: "尚无词条的书" }] } }),
    })));

    const wrapper = mount(HomePage, { global: { stubs: { RouterLink: true } } });
    await flushPromises();
    await wrapper.get(".discovery-query-field input").setValue("成长主题");
    await wrapper.get(".discovery-search").trigger("submit");
    await flushPromises();

    const result = wrapper.get(".discovery-book-link");
    expect(result.text()).toContain("尚无词条的书");
    expect(result.attributes("href")).toContain("https://zh.wikipedia.org/w/index.php?search=");
    expect(wrapper.text()).not.toContain("未获得引用来源");
  });
});
