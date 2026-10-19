<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type ComponentPublicInstance } from "vue";
import { onBeforeRouteLeave, RouterLink, useRoute } from "vue-router";
import { useVirtualizer } from "@tanstack/vue-virtual";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ArrowLeft, ArrowRight, Bookmark, BookOpen, Check, Copy, Highlighter, List, Settings2, Sparkles, X } from "lucide-vue-next";
import type { BookSummary, Chapter, ChapterContent, Highlight, Paragraph, ReadingAnchor } from "../domain/models";
import { usePreferencesStore } from "../app/stores/preferences";
import { useAiStore } from "../app/stores/ai";
import { canUseAiQuestion, validateAiQuestion, validateSelectedText } from "../domain/ai";
import { platform } from "../infrastructure/platform";

const route = useRoute();
const preferencesStore = usePreferencesStore();
const aiStore = useAiStore();
const preferences = computed(() => preferencesStore.value);
const aiCanExplain = computed(() => canUseAiQuestion(aiStore.settings, aiStore.connection, platform.isNative));
const aiUsageLimit = computed(() => /用量上限|subscription_sharing_usage_limit_exceeded/.test(aiError.value));
const book = ref<BookSummary | null>(null);
const chapters = ref<Chapter[]>([]);
const activeChapter = ref<Chapter | null>(null);
const paragraphs = ref<Paragraph[]>([]);
const readerScroller = ref<HTMLElement | null>(null);
const activeParagraphId = ref("");
const bookHighlights = ref<Highlight[]>([]);
const showContents = ref(false);
const loading = ref(true);
const errorMessage = ref("");
const saveError = ref("");
const readerStatus = ref("");
const selectedRange = ref<{ startParagraphId: string; endParagraphId: string; startIndex: number; endIndex: number; start: number; end: number; text: string } | null>(null);
const aiPanelOpen = ref(false);
const aiBusy = ref(false);
const aiResponse = ref("");
const aiError = ref("");
const aiQuestion = ref("");
const aiSourceText = ref("");
type AiQuestionSelection = { bookId: string; chapterId: string; startParagraphId: string; endParagraphId: string; startUtf16: number; endUtf16: number };
const aiSelection = ref<AiQuestionSelection | null>(null);
const aiSources = ref<Array<{ title: string; url: string }>>([]);
const aiPreparing = ref(false);
let activeAiRequestId = "";
let stopAiListener: UnlistenFn | undefined;
let sessionId = "";
let positionTimer: ReturnType<typeof setTimeout> | undefined;
let statusTimer: ReturnType<typeof setTimeout> | undefined;
let dwellParagraphId = "";
let dwellStartedAt = 0;
let saveQueue = Promise.resolve();
let lastSavedAnchor: ReadingAnchor | null = null;
let stopCloseHandler: (() => void) | undefined;
let closingWindow = false;
let layoutRestoreSequence = 0;
let lastVisibleAnchor: ReadingAnchor | null = null;
const scrollMargin = ref(0);

const virtualizer = useVirtualizer(computed(() => ({
  count: paragraphs.value.length,
  getScrollElement: () => readerScroller.value,
  estimateSize: () => Math.max(74, preferences.value.fontSizePx * preferences.value.lineHeight * 3.2),
  scrollMargin: scrollMargin.value,
  useAnimationFrameWithResizeObserver: true,
  overscan: 8,
  getItemKey: (index: number) => paragraphs.value[index]?.id ?? index,
})));
const virtualItems = computed(() => virtualizer.value.getVirtualItems());
const totalVirtualSize = computed(() => virtualizer.value.getTotalSize());
const currentChapterIndex = computed(() => chapters.value.findIndex((chapter) => chapter.id === activeChapter.value?.id));
const chapterProgress = computed(() => chapters.value.length ? Math.round(((Math.max(0, currentChapterIndex.value) + 1) / chapters.value.length) * 100) : 0);

function measureParagraph(element: Element | ComponentPublicInstance | null) {
  if (element instanceof HTMLElement) virtualizer.value.measureElement(element);
}

function measureVirtualListOffset() {
  if (!readerScroller.value) return 0;
  const list = readerScroller.value.querySelector<HTMLElement>(".reader-virtual-list");
  if (!list) return 0;
  return Math.max(0, list.getBoundingClientRect().top - readerScroller.value.getBoundingClientRect().top + readerScroller.value.scrollTop);
}

function nextLayoutFrame() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
}

function setStatus(message: string) {
  readerStatus.value = message;
  if (statusTimer) clearTimeout(statusTimer);
  statusTimer = setTimeout(() => { readerStatus.value = ""; }, 3200);
}

function anchorAtCurrentView(): ReadingAnchor | null {
  if (!book.value || !activeChapter.value || !readerScroller.value) return null;
  const scrollerRect = readerScroller.value.getBoundingClientRect();
  const readingY = Math.min(scrollerRect.bottom - 2, scrollerRect.top + 48);
  const renderedParagraphs = Array.from(readerScroller.value.querySelectorAll<HTMLElement>(".reader-paragraph"))
    .map((paragraphElement) => ({ element: paragraphElement, rect: paragraphElement.getBoundingClientRect() }))
    .filter(({ rect }) => rect.bottom > readingY && rect.top < scrollerRect.bottom);
  const element = renderedParagraphs
    .filter(({ rect }) => rect.top <= readingY)
    .sort((left, right) => right.rect.top - left.rect.top)[0]?.element
    ?? renderedParagraphs.sort((left, right) => left.rect.top - right.rect.top)[0]?.element
    ?? Array.from(readerScroller.value.querySelectorAll<HTMLElement>(".reader-paragraph"))
      .sort((left, right) => right.getBoundingClientRect().top - left.getBoundingClientRect().top)[0];
  const paragraphIndex = element ? Number(element.dataset.index) : -1;
  const paragraph = paragraphs.value[paragraphIndex];
  if (!paragraph || !element) return null;
  let intraParagraphUtf16 = 0;
  const elementRect = element.getBoundingClientRect();
  const x = Math.min(elementRect.right - 2, elementRect.left + Math.max(4, Math.min(40, elementRect.width / 6)));
  const y = Math.max(elementRect.top + 1, Math.min(readingY, elementRect.bottom - 1));
  const caretApi = document as Document & { caretPositionFromPoint?: (x: number, y: number) => CaretPosition | null };
  const caret = caretApi.caretPositionFromPoint?.(x, y);
  if (caret && element.contains(caret.offsetNode)) {
    intraParagraphUtf16 = Math.min(paragraph.text.length, offsetWithinParagraph(element, caret.offsetNode, caret.offset));
  }
  return {
    bookId: book.value.id,
    chapterId: activeChapter.value.id,
    paragraphId: paragraph.id,
    intraParagraphUtf16,
    absoluteUtf16: paragraph.startUtf16 + intraParagraphUtf16,
    nearbyText: paragraph.text.slice(intraParagraphUtf16, intraParagraphUtf16 + 48),
    parserVersion: 2,
  };
}

function persistAnchor(anchor: ReadingAnchor) {
  saveQueue = saveQueue.catch(() => undefined).then(async () => {
    await platform.invoke("save_reading_position", { anchor });
    lastSavedAnchor = anchor;
    saveError.value = "";
    if (book.value) book.value.progressRatio = Math.min(1, anchor.absoluteUtf16 / Math.max(1, book.value.totalUtf16));
  }).catch((error) => { saveError.value = `閱讀位置暫時未保存：${String(error)}`; });
  return saveQueue;
}

async function saveCurrentPosition() {
  const anchor = anchorAtCurrentView();
  if (!anchor) return;
  if (lastSavedAnchor && lastSavedAnchor.chapterId === anchor.chapterId && lastSavedAnchor.paragraphId === anchor.paragraphId && lastSavedAnchor.intraParagraphUtf16 === anchor.intraParagraphUtf16) return;
  await persistAnchor(anchor);
}

function schedulePositionSave() {
  if (positionTimer) clearTimeout(positionTimer);
  positionTimer = setTimeout(() => void saveCurrentPosition(), 420);
}

function recordDwell() {
  if (!book.value || !dwellParagraphId || !dwellStartedAt) return;
  const dwellMs = Date.now() - dwellStartedAt;
  const paragraphId = dwellParagraphId;
  dwellStartedAt = Date.now();
  if (dwellMs >= 1500) void platform.invoke("record_paragraph_view", { bookId: book.value.id, paragraphId, dwellMs }).catch(() => undefined);
}

async function restoreCharacterOffset(paragraphIndex: number, offset: number) {
  if (!readerScroller.value) return;
  await nextTick();
  const paragraphElement = readerScroller.value.querySelector<HTMLElement>(`[data-index="${paragraphIndex}"]`);
  if (!paragraphElement) return;
  if (offset <= 0) {
    const scroller = readerScroller.value.getBoundingClientRect();
    readerScroller.value.scrollTop += paragraphElement.getBoundingClientRect().top - scroller.top - 48;
    return;
  }
  const walker = document.createTreeWalker(paragraphElement, NodeFilter.SHOW_TEXT);
  let remaining = offset;
  let textNode = walker.nextNode();
  while (textNode) {
    const length = textNode.textContent?.length ?? 0;
    if (!(textNode.textContent ?? "").trim()) {
      textNode = walker.nextNode();
      continue;
    }
    if (remaining <= length) break;
    remaining -= length;
    textNode = walker.nextNode();
  }
  if (!textNode) return;
  const range = document.createRange();
  const textLength = textNode.textContent?.length ?? 0;
  let startOffset = Math.min(remaining, textLength);
  let endOffset = Math.min(startOffset + 1, textLength);
  if (endOffset === startOffset && startOffset > 0) startOffset -= 1;
  range.setStart(textNode, startOffset);
  range.setEnd(textNode, endOffset);
  const target = range.getBoundingClientRect();
  const scroller = readerScroller.value.getBoundingClientRect();
  readerScroller.value.scrollTop += target.top - scroller.top - 48;
}

async function openChapter(chapter: Chapter, paragraphId?: string, offset = 0) {
  await stopAiRequest();
  aiPanelOpen.value = false;
  selectedRange.value = null;
  recordDwell();
  lastVisibleAnchor = null;
  if (!readerScroller.value) loading.value = true;
  errorMessage.value = "";
  showContents.value = false;
  try {
    const content = await platform.invoke<ChapterContent | null>("get_chapter_content", { chapterId: chapter.id });
    if (!content) throw new Error("這一章暫時無法读取，请返回书架重试。");
    activeChapter.value = content.chapter;
    paragraphs.value = content.paragraphs;
    activeParagraphId.value = paragraphId ?? content.paragraphs[0]?.id ?? "";
    dwellParagraphId = activeParagraphId.value;
    dwellStartedAt = Date.now();
    if (!readerScroller.value) {
      loading.value = false;
      await nextTick();
    }
    await nextTick();
    scrollMargin.value = measureVirtualListOffset();
    await nextTick();
    virtualizer.value.measure();
    if (paragraphId) {
      const index = content.paragraphs.findIndex((paragraph) => paragraph.id === paragraphId);
      if (index >= 0) {
        virtualizer.value.scrollToIndex(index, { align: "start" });
        await restoreCharacterOffset(index, offset);
      }
      else if (readerScroller.value) readerScroller.value.scrollTop = 0;
    } else if (readerScroller.value) {
      readerScroller.value.scrollTop = 0;
      virtualizer.value.scrollToIndex(0, { align: "start" });
      await nextTick();
      await nextLayoutFrame();
      if (readerScroller.value) readerScroller.value.scrollTop = 0;
    }
    lastVisibleAnchor = anchorAtCurrentView();
    if (lastVisibleAnchor) activeParagraphId.value = lastVisibleAnchor.paragraphId;
    schedulePositionSave();
  } catch (error) {
    errorMessage.value = String(error);
    activeChapter.value = null;
    paragraphs.value = [];
  } finally {
    loading.value = false;
  }
}

async function loadBook() {
  loading.value = true;
  errorMessage.value = "";
  const bookId = String(route.params.bookId ?? "");
  try {
    const loadedBook = await platform.invoke<BookSummary | null>("get_book", { bookId });
    if (!loadedBook) {
      book.value = null;
      chapters.value = [];
      return;
    }
    book.value = loadedBook;
    chapters.value = await platform.invoke<Chapter[]>("list_chapters", { bookId });
    if (!chapters.value.length) throw new Error("这本书没有可读取的章节，请重新导入 TXT。");
    bookHighlights.value = await platform.invoke<Highlight[]>("list_highlights", { bookId });
    const saved = await platform.invoke<ReadingAnchor | null>("get_reading_position", { bookId });
    const queryChapterId = typeof route.query.chapterId === "string" ? route.query.chapterId : "";
    const queryParagraphId = typeof route.query.paragraphId === "string" ? route.query.paragraphId : "";
    const requestedOffset = typeof route.query.offset === "string" ? Number.parseInt(route.query.offset, 10) : 0;
    const resumeChapterId = queryChapterId || saved?.chapterId || "";
    const resumeChapter = chapters.value.find((chapter) => chapter.id === resumeChapterId) ?? chapters.value[0]!;
    const resumeParagraphId = queryParagraphId || (resumeChapter.id === saved?.chapterId ? saved.paragraphId : "");
    const resumeOffset = queryParagraphId ? Math.max(0, Number.isFinite(requestedOffset) ? requestedOffset : 0) : resumeChapter.id === saved?.chapterId ? saved.intraParagraphUtf16 : 0;
    await platform.invoke("touch_book", { bookId });
    await openChapter(resumeChapter, resumeParagraphId || undefined, resumeOffset);
    await nextTick();
    const initialAnchor = anchorAtCurrentView();
    sessionId = await platform.invoke<string>("start_reading_session", { bookId, anchor: initialAnchor });
    if (!saved && initialAnchor) await persistAnchor(initialAnchor);
    if (platform.isNative) await installCloseHandler();
  } catch (error) {
    errorMessage.value = String(error);
    book.value = null;
  } finally {
    loading.value = false;
  }
}

function moveChapter(direction: -1 | 1) {
  const next = chapters.value[currentChapterIndex.value + direction];
  if (next) void openChapter(next);
}

function onReaderScroll() {
  if (!readerScroller.value) return;
  const anchor = anchorAtCurrentView();
  if (anchor) {
    lastVisibleAnchor = anchor;
    if (anchor.paragraphId !== activeParagraphId.value) {
      recordDwell();
      activeParagraphId.value = anchor.paragraphId;
      dwellParagraphId = anchor.paragraphId;
      dwellStartedAt = Date.now();
    }
    schedulePositionSave();
  }
}

function paragraphElement(node: Node): HTMLElement | null {
  const element = node instanceof Element ? node : node.parentElement;
  return element?.closest<HTMLElement>(".reader-paragraph") ?? null;
}

function offsetWithinParagraph(element: HTMLElement, node: Node, offset: number) {
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  let current = walker.nextNode();
  let total = 0;
  while (current) {
    const text = current.textContent ?? "";
    if (current === node) return total + offset;
    if (text.trim()) total += text.length;
    current = walker.nextNode();
  }
  return total;
}

function onSelectionChange() {
  selectedRange.value = null;
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || !selection.toString().trim()) return;
  const selectedDomRange = selection.getRangeAt(0);
  const startElement = paragraphElement(selectedDomRange.startContainer);
  const endElement = paragraphElement(selectedDomRange.endContainer);
  if (!startElement || !endElement || !readerScroller.value?.contains(startElement) || !readerScroller.value.contains(endElement)) return;
  const startIndex = Number(startElement.dataset.index);
  const endIndex = Number(endElement.dataset.index);
  if (endIndex < startIndex) return;
  const startParagraph = paragraphs.value[startIndex];
  const endParagraph = paragraphs.value[endIndex];
  if (!startParagraph || !endParagraph) return;
  const start = offsetWithinParagraph(startElement, selectedDomRange.startContainer, selectedDomRange.startOffset);
  const end = offsetWithinParagraph(endElement, selectedDomRange.endContainer, selectedDomRange.endOffset);
  const renderedSelection = paragraphs.value.slice(startIndex, endIndex + 1).map((paragraph, index, selected) => {
    const from = index === 0 ? start : 0;
    const to = index === selected.length - 1 ? end : paragraph.text.length;
    return paragraph.text.slice(from, to);
  }).join("");
  if (!renderedSelection || renderedSelection !== selection.toString()) return;
  let sourceSnapshot = "";
  for (let index = startIndex; index <= endIndex; index += 1) {
    const paragraph = paragraphs.value[index]!;
    if (index > startIndex) sourceSnapshot += "\n".repeat(Math.max(0, paragraph.startUtf16 - paragraphs.value[index - 1]!.endUtf16));
    sourceSnapshot += paragraph.text.slice(index === startIndex ? start : 0, index === endIndex ? end : paragraph.text.length);
  }
  if (!sourceSnapshot || sourceSnapshot.length > 16_384) {
    setStatus("所选原文太长，请缩短范围后再摘录。");
    return;
  }
  selectedRange.value = { startParagraphId: startParagraph.id, endParagraphId: endParagraph.id, startIndex, endIndex, start, end, text: sourceSnapshot };
}

async function addCurrentBookmark() {
  const anchor = anchorAtCurrentView();
  if (!anchor) return;
  try {
    const inserted = await platform.invoke<boolean>("add_bookmark", { bookId: anchor.bookId, paragraphId: anchor.paragraphId, intraUtf16: anchor.intraParagraphUtf16, note: null });
    setStatus(inserted ? "已添加书签。" : "这个位置已有书签。可在“书签与摘录”中查看或移除。");
  } catch (error) { saveError.value = `添加书签失败：${String(error)}`; }
}

async function saveSelection() {
  if (!selectedRange.value || !book.value) return;
  try {
    const highlight = await platform.invoke<Highlight | null>("add_highlight", { bookId: book.value.id, startParagraphId: selectedRange.value.startParagraphId, startIntraUtf16: selectedRange.value.start, endParagraphId: selectedRange.value.endParagraphId, endIntraUtf16: selectedRange.value.end });
    if (!highlight || highlight.textSnapshot !== selectedRange.value.text) throw new Error("原文范围校验未通过，摘录没有保存。");
    bookHighlights.value.unshift(highlight);
    window.getSelection()?.removeAllRanges();
    selectedRange.value = null;
    setStatus("摘录已保存，可在“书签与摘录”中回到原文。");
  } catch (error) { saveError.value = `保存摘录失败：${String(error)}`; }
}

async function copySelection() {
  if (!selectedRange.value) return;
  try {
    await navigator.clipboard.writeText(selectedRange.value.text);
    setStatus("已复制选中的原文。");
  } catch (error) { saveError.value = `复制失败：${String(error)}`; }
}

function paragraphParts(paragraph: Paragraph) {
  const paragraphIndex = paragraphs.value.findIndex((item) => item.id === paragraph.id);
  const ranges = bookHighlights.value.flatMap((highlight) => {
    const startIndex = paragraphs.value.findIndex((item) => item.id === highlight.startParagraphId);
    const endIndex = paragraphs.value.findIndex((item) => item.id === highlight.endParagraphId);
    if (startIndex < 0 || endIndex < startIndex || paragraphIndex < startIndex || paragraphIndex > endIndex) return [];
    return [{ start: paragraphIndex === startIndex ? highlight.startIntraUtf16 : 0, end: paragraphIndex === endIndex ? highlight.endIntraUtf16 : paragraph.text.length, id: highlight.id }];
  }).sort((a, b) => a.start - b.start);
  if (!ranges.length) return [{ key: "text", text: paragraph.text, highlighted: false }];
  const parts: Array<{ key: string; text: string; highlighted: boolean }> = [];
  let cursor = 0;
  for (const [index, range] of ranges.entries()) {
    const start = Math.max(cursor, Math.min(paragraph.text.length, range.start));
    const end = Math.max(start, Math.min(paragraph.text.length, range.end));
    if (start > cursor) parts.push({ key: `plain-${index}`, text: paragraph.text.slice(cursor, start), highlighted: false });
    if (end > start) parts.push({ key: `mark-${range.id}-${index}`, text: paragraph.text.slice(start, end), highlighted: true });
    cursor = end;
  }
  if (cursor < paragraph.text.length) parts.push({ key: "plain-end", text: paragraph.text.slice(cursor), highlighted: false });
  return parts;
}

async function finishSession() {
  await stopAiRequest();
  if (positionTimer) { clearTimeout(positionTimer); positionTimer = undefined; }
  recordDwell();
  await saveCurrentPosition();
  if (sessionId) {
    const id = sessionId;
    sessionId = "";
    await platform.invoke("end_reading_session", { sessionId: id, anchor: anchorAtCurrentView() }).catch((error) => { saveError.value = `阅读记录暂时未结束：${String(error)}`; });
  }
}

type AiStreamPayload = { requestId: string; kind: "token" | "citation" | "complete" | "cancelled" | "error"; text?: string; title?: string; url?: string; message?: string };

function openAiPanel() {
  if (!selectedRange.value || !book.value || !activeChapter.value) return;
  aiSelection.value = {
    bookId: book.value.id,
    chapterId: activeChapter.value.id,
    startParagraphId: selectedRange.value.startParagraphId,
    endParagraphId: selectedRange.value.endParagraphId,
    startUtf16: selectedRange.value.start,
    endUtf16: selectedRange.value.end,
  };
  try {
    aiSourceText.value = validateSelectedText(selectedRange.value.text);
  } catch (error) {
    setStatus(String(error));
    return;
  }
  if (!aiCanExplain.value) {
    setStatus("请先连接ChatGPT账号并选择可用模型。");
    return;
  }
  aiPanelOpen.value = true;
  aiResponse.value = "";
  aiError.value = "";
  aiQuestion.value = "";
  aiSources.value = [];
}

async function askAiQuestion() {
  const selection = aiSelection.value;
  if (!aiSourceText.value || !selection || !aiCanExplain.value || aiBusy.value || aiPreparing.value) return;
  let question: string;
  try {
    question = validateAiQuestion(aiQuestion.value);
  } catch (error) {
    aiError.value = String(error);
    return;
  }
  aiError.value = "";
  aiPreparing.value = true;
  try {
    await saveCurrentPositionForAi(selection);
    await submitAiQuestion(question, selection);
  } catch (error) {
    aiError.value = String(error);
  } finally {
    aiPreparing.value = false;
  }
}

async function saveCurrentPositionForAi(selection: AiQuestionSelection) {
  const selectedEndParagraph = paragraphs.value.find((paragraph) => paragraph.id === selection.endParagraphId);
  if (!selectedEndParagraph || selectedEndParagraph.chapterId !== selection.chapterId) {
    throw new Error("提问选段已不在当前章节；请重新选择原文后重试。");
  }
  const selectedEndUtf16 = selection.endUtf16;
  if (selectedEndUtf16 < 0 || selectedEndUtf16 > selectedEndParagraph.text.length) {
    throw new Error("提问选段位置无效；请重新选择原文后重试。");
  }
  const selectionAnchor: ReadingAnchor = {
    bookId: selection.bookId,
    chapterId: selection.chapterId,
    paragraphId: selectedEndParagraph.id,
    intraParagraphUtf16: selectedEndUtf16,
    absoluteUtf16: selectedEndParagraph.startUtf16 + selectedEndUtf16,
    nearbyText: selectedEndParagraph.text.slice(selectedEndUtf16, selectedEndUtf16 + 48),
    parserVersion: 2,
  };
  const visibleAnchor = anchorAtCurrentView();
  const anchor = visibleAnchor && visibleAnchor.absoluteUtf16 > selectionAnchor.absoluteUtf16
    ? visibleAnchor
    : selectionAnchor;
  if (positionTimer) {
    clearTimeout(positionTimer);
    positionTimer = undefined;
  }
  await saveQueue.catch(() => undefined);
  try {
    await platform.invoke("save_reading_position", { anchor });
    lastSavedAnchor = anchor;
    saveError.value = "";
  } catch (error) {
    saveError.value = `阅读位置保存失败：${String(error)}`;
    throw new Error("阅读位置未能保存，已读上下文因此未发送。请检查本机数据状态后重试。");
  }
}

async function submitAiQuestion(question: string, selection: AiQuestionSelection) {
  await stopAiRequest();
  const requestId = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  activeAiRequestId = requestId;
  aiPanelOpen.value = true;
  aiBusy.value = true;
  aiResponse.value = "";
  aiError.value = "";
  aiSources.value = [];
  try {
    stopAiListener = await listen<AiStreamPayload>("shuyu://ai-stream", (event) => {
      const payload = event.payload;
      if (payload.requestId !== activeAiRequestId) return;
      if (payload.kind === "token") aiResponse.value += payload.text ?? "";
      if (payload.kind === "citation" && payload.url && /^https?:\/\//i.test(payload.url)) {
        if (!aiSources.value.some((source) => source.url === payload.url)) {
          aiSources.value.push({ title: payload.title?.trim() || payload.url, url: payload.url });
        }
      }
      if (payload.kind === "complete") aiBusy.value = false;
      if (payload.kind === "cancelled") { aiBusy.value = false; aiSources.value = []; aiError.value = "已取消解释。"; }
      if (payload.kind === "error") { aiBusy.value = false; aiSources.value = []; aiError.value = payload.message ?? "模型请求失败，请稍后重试。"; }
      if (payload.kind !== "token") {
        activeAiRequestId = "";
        stopAiListener?.();
        stopAiListener = undefined;
      }
    });
    await platform.invoke("start_chatgpt_ai_explanation", {
      requestId,
      selectedText: aiSourceText.value,
      question,
      ...selection,
    });
  } catch (error) {
    aiBusy.value = false;
    aiSources.value = [];
    aiError.value = String(error);
    activeAiRequestId = "";
    stopAiListener?.();
    stopAiListener = undefined;
  }
}

async function openAiUsageManager() {
  try {
    await platform.invoke("open_chatgpt_usage");
  } catch (error) {
    aiError.value = String(error);
  }
}

async function closeAiPanel() {
  aiPanelOpen.value = false;
  await stopAiRequest();
}

async function stopAiRequest() {
  const requestId = activeAiRequestId;
  activeAiRequestId = "";
  if (requestId && aiBusy.value && platform.isNative) {
    await platform.invoke<boolean>("cancel_ai_request", { requestId }).catch(() => false);
  }
  aiBusy.value = false;
  stopAiListener?.();
  stopAiListener = undefined;
}

async function installCloseHandler() {
  const windowHandle = getCurrentWindow();
  stopCloseHandler?.();
  stopCloseHandler = await windowHandle.onCloseRequested(async (event) => {
    event.preventDefault();
    if (closingWindow) return;
    closingWindow = true;
    try {
      await finishSession();
      await windowHandle.destroy();
    } catch (error) {
      closingWindow = false;
      saveError.value = `窗口暂时无法关闭：${String(error)}`;
    }
  });
}

function handlePageHide() { void finishSession(); }
onMounted(() => {
  window.addEventListener("pagehide", handlePageHide);
  void loadBook();
});
onBeforeRouteLeave(async () => { await finishSession(); });
watch(() => [route.params.bookId, route.query.chapterId, route.query.paragraphId], async () => {
  if (sessionId) await finishSession();
  void loadBook();
});
onBeforeUnmount(() => {
  window.removeEventListener("pagehide", handlePageHide);
  stopCloseHandler?.();
  if (positionTimer) clearTimeout(positionTimer);
  if (statusTimer) clearTimeout(statusTimer);
  void finishSession();
});

async function restoreLayoutAnchor(anchor = lastVisibleAnchor ?? anchorAtCurrentView()) {
  if (!anchor) {
    scrollMargin.value = measureVirtualListOffset();
    await nextTick();
    virtualizer.value.measure();
    return;
  }
  const sequence = ++layoutRestoreSequence;
  await nextTick();
  if (sequence !== layoutRestoreSequence || !readerScroller.value) return;
  scrollMargin.value = measureVirtualListOffset();
  await nextTick();
  if (sequence !== layoutRestoreSequence) return;
  const paragraphIndex = paragraphs.value.findIndex((paragraph) => paragraph.id === anchor.paragraphId);
  if (paragraphIndex < 0) return;
  virtualizer.value.measure();
  await nextTick();
  await nextLayoutFrame();
  if (sequence !== layoutRestoreSequence) return;
  virtualizer.value.scrollToIndex(paragraphIndex, { align: "start" });
  await nextTick();
  if (sequence !== layoutRestoreSequence) return;
  await restoreCharacterOffset(paragraphIndex, anchor.intraParagraphUtf16);
  await nextLayoutFrame();
  if (sequence !== layoutRestoreSequence) return;
  await restoreCharacterOffset(paragraphIndex, anchor.intraParagraphUtf16);
  if (sequence === layoutRestoreSequence) {
    lastVisibleAnchor = anchor;
    schedulePositionSave();
  }
}

watch(
  () => [preferences.value.fontSizePx, preferences.value.lineHeight, preferences.value.contentWidthPx],
  () => void restoreLayoutAnchor(anchorAtCurrentView()),
  { flush: "pre" },
);
watch(() => route.params.bookId, () => void loadBook());
</script>

<template>
  <section class="reader-page" @keydown.esc="showContents = false">
    <header class="reader-toolbar">
      <div class="reader-book-meta">
        <RouterLink to="/library" class="reader-back" aria-label="返回书架"><ArrowLeft :size="18" /></RouterLink>
        <div><strong>{{ book?.title ?? "正在打开" }}</strong><span>{{ activeChapter?.title ?? "本地 TXT 阅读器" }}</span></div>
      </div>
      <div class="reader-toolbar-actions">
        <span class="reader-progress-label">第{{ Math.max(1, currentChapterIndex + 1) }} / {{ chapters.length || 1 }}章</span>
        <button class="reader-icon-button" type="button" aria-label="打开章节目录" :aria-expanded="showContents" @click="showContents = !showContents"><List :size="18" /></button>
        <button class="reader-icon-button" type="button" aria-label="为当前段落添加书签" @click="addCurrentBookmark"><Bookmark :size="17" /></button>
        <details class="reader-settings">
          <summary class="reader-icon-button" aria-label="设置"><Settings2 :size="18" /></summary>
          <div class="reader-settings-popover">
            <label>字号 <span>{{ preferences.fontSizePx }} px</span><input :value="preferences.fontSizePx" type="range" min="14" max="28" @input="preferencesStore.update({ fontSizePx: Number(($event.target as HTMLInputElement).value) })" /></label>
            <label>行距 <span>{{ preferences.lineHeight.toFixed(2) }}</span><input :value="preferences.lineHeight" type="range" min="1.4" max="2.2" step="0.05" @input="preferencesStore.update({ lineHeight: Number(($event.target as HTMLInputElement).value) })" /></label>
            <label>正文宽度 <span>{{ preferences.contentWidthPx }} px</span><input :value="preferences.contentWidthPx" type="range" min="540" max="860" step="10" @input="preferencesStore.update({ contentWidthPx: Number(($event.target as HTMLInputElement).value) })" /></label>
            <RouterLink to="/settings" class="quiet-link">设置 <ArrowRight :size="14" /></RouterLink>
          </div>
        </details>
      </div>
    </header>

    <div class="reader-progress-track" aria-hidden="true"><i :style="{ width: `${chapterProgress}%` }"></i></div>
    <div v-if="readerStatus || saveError" class="reader-feedback" role="status"><span>{{ saveError || readerStatus }}</span><button type="button" aria-label="关闭提示" @click="saveError = ''; readerStatus = ''"><Check :size="14" /></button></div>

    <div v-if="loading" class="reader-state"><span class="progress-spinner"></span><p>正在载入这一章…</p></div>
    <div v-else-if="errorMessage" class="reader-state reader-state-error" role="status"><BookOpen :size="24" /><h2>暂时无法打开这本书</h2><p>{{ errorMessage }}</p><RouterLink class="button button-secondary" to="/library">回到书架</RouterLink></div>
    <template v-else-if="book && activeChapter">
      <div class="reader-scene">
        <aside v-if="showContents" class="reader-toc" aria-label="章节目录">
          <div class="reader-toc-heading"><div><span class="eyebrow subdued">本书目录</span><h2>章节</h2></div><button class="reader-icon-button" type="button" aria-label="关闭目录" @click="showContents = false">×</button></div>
          <nav><button v-for="chapter in chapters" :key="chapter.id" type="button" class="reader-toc-item" :class="{ selected: chapter.id === activeChapter.id }" :aria-current="chapter.id === activeChapter.id ? 'page' : undefined" @click="openChapter(chapter)"><small>{{ String(chapter.ordinal + 1).padStart(2, '0') }}</small><span>{{ chapter.title }}</span></button></nav>
        </aside>
        <main ref="readerScroller" class="reader-scroller" tabindex="0" aria-label="书籍正文" @scroll.passive="onReaderScroll" @mouseup="onSelectionChange" @keyup="onSelectionChange">
          <article class="reader-content" :style="{ maxWidth: `${preferences.contentWidthPx}px`, fontSize: `${preferences.fontSizePx}px`, lineHeight: preferences.lineHeight }">
            <header class="reader-chapter-heading"><span class="eyebrow subdued">{{ book.title }}</span><h1>{{ activeChapter.title }}</h1><span>{{ activeChapter.ordinal + 1 }} / {{ chapters.length }} 章</span></header>
            <div class="reader-virtual-list" :style="{ height: `${totalVirtualSize}px` }">
              <p v-for="item in virtualItems" :key="String(item.key)" :ref="measureParagraph" :data-index="item.index" class="reader-paragraph" :class="{ 'paragraph-active': paragraphs[item.index]?.id === activeParagraphId }" :style="{ transform: `translateY(${item.start - scrollMargin}px)` }"><template v-for="part in paragraphParts(paragraphs[item.index]!)" :key="part.key"><mark v-if="part.highlighted" class="reader-saved-highlight">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template></p>
            </div>
            <footer class="reader-chapter-footer">
              <button class="button button-secondary" type="button" :disabled="currentChapterIndex <= 0" @click="moveChapter(-1)"><ArrowLeft :size="15" /> 上一章</button>
              <span v-if="currentChapterIndex === chapters.length - 1">本章读完，故事也到这里了。</span>
              <span v-else>读完这一章，再继续下一页。</span>
              <button class="button button-secondary" type="button" :disabled="currentChapterIndex >= chapters.length - 1" @click="moveChapter(1)">下一章 <ArrowRight :size="15" /></button>
            </footer>
          </article>
        </main>
        <div v-if="selectedRange" class="reader-selection-toolbar" role="toolbar" aria-label="选中文字操作">
          <span><Highlighter :size="15" /> 已选中原文</span>
          <button type="button" @click="copySelection"><Copy :size="14" /> 复制</button>
          <button type="button" @click="saveSelection"><Bookmark :size="14" /> 摘录</button>
          <button v-if="platform.isNative" type="button" :disabled="!aiCanExplain || aiBusy || aiPreparing" title="回答将结合已读范围并使用ChatGPT联网检索背景资料" @click="openAiPanel"><Sparkles :size="14" /> AI问答</button>
        </div>
        <section v-if="aiPanelOpen" class="reader-ai-panel" aria-label="AI问答" aria-live="polite">
          <header><div><Sparkles :size="16" /><strong>AI问答</strong></div><div class="reader-ai-panel-actions"><button type="button" aria-label="关闭AI解释" @click="closeAiPanel"><X :size="16" /></button></div></header>
          <form class="reader-ai-question-form" @submit.prevent="askAiQuestion">
            <textarea id="reader-ai-question" v-model="aiQuestion" aria-label="输入问题" maxlength="1000" rows="2" :disabled="aiBusy || aiPreparing"></textarea>
            <div class="reader-ai-question-actions"><small>{{ aiQuestion.length }}/1,000</small><button class="button button-primary" type="submit" :disabled="aiBusy || aiPreparing">{{ aiPreparing ? "正在准备…" : aiBusy ? "正在回答…" : aiQuestion.trim() ? "发送问题" : "提问" }}</button></div>
          </form>
          <p v-if="aiResponse" class="reader-ai-response">{{ aiResponse }}</p>
          <ul v-if="aiSources.length" class="reader-ai-sources" aria-label="联网来源"><li v-for="source in aiSources" :key="source.url"><a :href="source.url" target="_blank" rel="noopener noreferrer">{{ source.title }}</a></li></ul>
          <p v-if="aiError" class="reader-ai-error" role="status">{{ aiError }}</p>
          <button v-if="aiUsageLimit" class="quiet-link reader-ai-usage-link" type="button" @click="openAiUsageManager">打开ChatGPT Manage usage</button>
        </section>
      </div>
    </template>
  </section>
</template>
