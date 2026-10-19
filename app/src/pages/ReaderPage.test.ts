// @vitest-environment jsdom
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { reactive } from "vue";
import { defaultAiSettings, type AiSettings } from "../domain/ai";

const mocks = vi.hoisted(() => ({
  route: { params: { bookId: "book-1" }, query: {} as Record<string, string> },
  preferencesStore: null as unknown,
  aiStore: null as unknown,
  invoke: vi.fn(),
  listen: vi.fn(),
  virtualizer: { scrollToIndex: vi.fn() },
  windowApi: {
    onCloseRequested: vi.fn(async () => vi.fn()),
    destroy: vi.fn(async () => undefined),
  },
}));

vi.mock("vue-router", () => ({
  useRoute: () => mocks.route,
  onBeforeRouteLeave: () => undefined,
  RouterLink: { props: ["to"], template: "<a :href=\"to\"><slot /></a>" },
}));
vi.mock("@tanstack/vue-virtual", () => ({
  useVirtualizer: () => ({
    value: {
      getVirtualItems: () => [{ key: "p1", index: 0, start: 0, end: 80, size: 80 }],
      getTotalSize: () => 80,
      measureElement: vi.fn(),
      measure: vi.fn(),
      scrollToIndex: mocks.virtualizer.scrollToIndex,
    },
  }),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => mocks.windowApi }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("../app/stores/preferences", () => ({ usePreferencesStore: () => mocks.preferencesStore }));
vi.mock("../app/stores/ai", () => ({ useAiStore: () => mocks.aiStore }));
vi.mock("../infrastructure/platform", () => ({
  platform: { get isNative() { return true; }, invoke: mocks.invoke },
}));

import ReaderPage from "./ReaderPage.vue";

const book = {
  id: "book-1",
  title: "合成测试书",
  author: "测试作者",
  importName: "fixture.txt",
  chapterCount: 1,
  totalUtf16: 22,
  createdAt: "2026-10-10T00:00:00Z",
  lastOpenedAt: null,
  progressRatio: 0,
};
const chapter = {
  id: "chapter-1",
  bookId: "book-1",
  ordinal: 0,
  title: "第一章",
  startUtf16: 0,
  endUtf16: 22,
};
const nextChapter = {
  id: "chapter-2",
  bookId: "book-1",
  ordinal: 1,
  title: "第二章",
  startUtf16: 22,
  endUtf16: 44,
};
const paragraph = {
  id: "p1",
  bookId: "book-1",
  chapterId: "chapter-1",
  ordinal: 0,
  text: "SOURCE_TEXT 旧信一直保留。",
  startUtf16: 0,
  endUtf16: "SOURCE_TEXT 旧信一直保留。".length,
};
const nextParagraph = {
  ...paragraph,
  id: "p2",
  chapterId: "chapter-2",
  text: "SOURCE_TEXT 第二章内容。",
};
let wrapper: VueWrapper | undefined;
let liveAiSettings: AiSettings;
let aiStreamHandler: ((event: { payload: { requestId: string; kind: string; text?: string; title?: string; url?: string } }) => void) | undefined;

function mountReader() {
  wrapper = mount(ReaderPage);
  return wrapper;
}

async function selectReaderText(target: VueWrapper) {
  const paragraphElement = target.get(".reader-paragraph").element;
  const walker = document.createTreeWalker(paragraphElement, NodeFilter.SHOW_TEXT);
  const textNodes: Text[] = [];
  let current = walker.nextNode();
  while (current) {
    if ((current.textContent ?? "").length > 0) textNodes.push(current as Text);
    current = walker.nextNode();
  }
  const startNode = textNodes[0];
  const endNode = textNodes[textNodes.length - 1];
  if (!startNode || !endNode) throw new Error("reader paragraph has no text node");
  const range = document.createRange();
  range.setStart(startNode, 0);
  range.setEnd(endNode, endNode.textContent?.length ?? 0);
  const selectedText = paragraphElement.textContent ?? "";
  vi.spyOn(window, "getSelection").mockReturnValue({
    isCollapsed: false,
    toString: () => selectedText,
    getRangeAt: () => range,
  } as unknown as Selection);
  await target.get(".reader-scroller").trigger("mouseup");
}

describe("Reader page expanded AI context flow", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.route = { params: { bookId: "book-1" }, query: {} };
    mocks.preferencesStore = {
      value: { theme: "paper", fontSizePx: 18, lineHeight: 1.85, contentWidthPx: 720 },
      update: vi.fn(),
    };
    liveAiSettings = reactive({
      ...defaultAiSettings,
      enabled: true,
      selectedModelSlug: "gpt-example",
      allowAiQa: true,
      privacyNoticeVersion: 2,
      privacyNoticeAcceptedAt: "2026-10-10T00:00:00Z",
    });
    mocks.aiStore = {
      settings: liveAiSettings,
      connection: { connected: true, planUsageAuthorized: true },
    };
    aiStreamHandler = undefined;
    mocks.listen.mockImplementation(async (_event: string, handler: typeof aiStreamHandler) => {
      aiStreamHandler = handler ?? undefined;
      return vi.fn();
    });
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "get_book") return book;
      if (command === "list_chapters") return [chapter];
      if (command === "list_highlights") return [];
      if (command === "get_reading_position") return null;
      if (command === "get_chapter_content") return { chapter, paragraphs: [paragraph] };
      if (command === "start_reading_session") return "session-1";
      if (command === "cancel_ai_request") return false;
      return undefined;
    });
  });

  afterEach(() => {
    wrapper?.unmount();
    wrapper = undefined;
    vi.restoreAllMocks();
  });

  it("loads the book once so the reader has one stable reading position", async () => {
    mountReader();
    await flushPromises();

    expect(mocks.invoke.mock.calls.filter(([command]) => command === "get_book")).toHaveLength(1);
  });

  it("moves to the top of the text when opening the next chapter", async () => {
    mocks.invoke.mockImplementation(async (command: string, args?: { chapterId?: string }) => {
      if (command === "get_book") return book;
      if (command === "list_chapters") return [chapter, nextChapter];
      if (command === "list_highlights") return [];
      if (command === "get_reading_position") return null;
      if (command === "get_chapter_content") {
        return args?.chapterId === nextChapter.id
          ? { chapter: nextChapter, paragraphs: [nextParagraph] }
          : { chapter, paragraphs: [paragraph] };
      }
      if (command === "start_reading_session") return "session-1";
      if (command === "cancel_ai_request") return false;
      return undefined;
    });
    const target = mountReader();
    await flushPromises();
    const scroller = target.get(".reader-scroller").element as HTMLElement;
    scroller.scrollTop = 500;

    await target.get(".reader-chapter-footer button:last-child").trigger("click");
    await flushPromises();

    expect(target.get(".reader-chapter-heading h1").text()).toBe("第二章");
    expect(scroller.scrollTop).toBe(0);
    expect(mocks.virtualizer.scrollToIndex).toHaveBeenCalledWith(0, { align: "start" });
  });

  it("sends the selected question directly with its saved reading position and no preview", async () => {
    const target = mountReader();
    await flushPromises();
    await selectReaderText(target);
    await target.get(".reader-selection-toolbar button:last-child").trigger("click");
    expect(target.find(".reader-ai-disclosure").exists()).toBe(false);
    expect(target.find(".reader-ai-note").exists()).toBe(false);
    expect(target.get(".reader-ai-panel").text()).not.toContain("发送后，选段");
    await target.get("#reader-ai-question").setValue("旧信为何重要？");
    await target.get(".reader-ai-question-form").trigger("submit");
    await flushPromises();

    expect(target.get(".reader-ai-panel").attributes("aria-label")).toBe("AI问答");
    expect(target.get(".reader-ai-panel").text()).toContain("AI问答");
    expect(target.get(".reader-ai-panel").text()).not.toContain("选段");
    expect(target.get(".reader-ai-panel").text()).not.toContain("想问这段原文什么");
    expect(target.find(".reader-ai-wait").exists()).toBe(false);
    expect(target.get("#reader-ai-question").attributes("aria-label")).toBe("输入问题");
    expect(target.find("button[aria-label='取消AI解释']").exists()).toBe(false);

    expect(mocks.invoke).toHaveBeenCalledWith("save_reading_position", expect.objectContaining({
      anchor: expect.objectContaining({
        paragraphId: paragraph.id,
        intraParagraphUtf16: paragraph.text.length,
      }),
    }));
    expect(mocks.invoke.mock.calls.some(([command]) => command === "preview_chatgpt_ai_explanation")).toBe(false);
    expect(target.find(".reader-ai-context-preview").exists()).toBe(false);
    const startCall = mocks.invoke.mock.calls.find(([command]) => command === "start_chatgpt_ai_explanation");
    expect(startCall).toBeDefined();
    expect(startCall?.[1]).toEqual(expect.objectContaining({
      bookId: "book-1",
      chapterId: "chapter-1",
      startParagraphId: paragraph.id,
      endParagraphId: paragraph.id,
      selectedText: paragraph.text,
      question: "旧信为何重要？",
    }));
    expect(startCall?.[1]).not.toHaveProperty("contextPreviewDigest");
  });

  it("shows only validated clickable sources returned with the completed response", async () => {
    const target = mountReader();
    await flushPromises();
    await selectReaderText(target);
    await target.get(".reader-selection-toolbar button:last-child").trigger("click");
    await target.get("#reader-ai-question").setValue("旧信为何重要？");
    await target.get(".reader-ai-question-form").trigger("submit");
    await flushPromises();
    const startCall = mocks.invoke.mock.calls.find(([command]) => command === "start_chatgpt_ai_explanation");
    const requestId = startCall?.[1]?.requestId as string;
    aiStreamHandler?.({ payload: { requestId, kind: "token", text: "回答" } });
    aiStreamHandler?.({ payload: { requestId, kind: "citation", title: "可信来源", url: "https://example.org/source" } });
    aiStreamHandler?.({ payload: { requestId, kind: "complete" } });
    await flushPromises();

    const source = target.get(".reader-ai-sources a");
    expect(source.text()).toBe("可信来源");
    expect(source.attributes("href")).toBe("https://example.org/source");
    expect(source.attributes("rel")).toBe("noopener noreferrer");
  });

  it("keeps the table of contents free of the redundant return-to-shelf footer", async () => {
    const target = mountReader();
    await flushPromises();
    await target.get("button[aria-label='打开章节目录']").trigger("click");

    expect(target.get(".reader-toc").text()).toContain("第一章");
    expect(target.find(".reader-toc-shelf").exists()).toBe(false);
    expect(target.get(".reader-toc").text()).not.toContain("返回书架");
  });

});
