import type { BookSummary, Bookmark, Chapter, ChapterContent, DailyQuote, Highlight, ImportResult, Paragraph, ReadingAnchor } from "../domain/models";

const booksKey = "shuyu.preview.books";
const contentKey = "shuyu.preview.content";
const positionsKey = "shuyu.preview.positions";
const bookmarksKey = "shuyu.preview.bookmarks";
const highlightsKey = "shuyu.preview.highlights";
const viewsKey = "shuyu.preview.views";
const sessionsKey = "shuyu.preview.sessions";
const quotesKey = "shuyu.preview.dailyQuotes";

function readBooks(): BookSummary[] {
  try { return JSON.parse(localStorage.getItem(booksKey) ?? "[]") as BookSummary[]; }
  catch { return []; }
}

function readContent(): Record<string, string> {
  try { return JSON.parse(localStorage.getItem(contentKey) ?? "{}") as Record<string, string>; }
  catch { return {}; }
}

function readStored<T>(key: string, fallback: T): T {
  try { return JSON.parse(localStorage.getItem(key) ?? "null") as T ?? fallback; }
  catch { return fallback; }
}

function newId() {
  return globalThis.crypto?.randomUUID?.() ?? `preview-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function localDateKey(date = new Date()) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

function extractPreviewSentence(text: string) {
  const spans: Array<[number, number]> = [];
  let start = 0;
  let units = 0;
  let terminal = false;
  for (const character of text) {
    const previousUnits = units;
    units += character.length;
    if (terminal && !"”’」』》】)）".includes(character) && !"。！？.!?".includes(character)) {
      spans.push([start, previousUnits]);
      start = previousUnits;
      terminal = false;
    }
    if ("。！？.!?".includes(character)) terminal = true;
  }
  if (start < units) spans.push([start, units]);
  for (const [spanStart, spanEnd] of spans) {
    const raw = text.slice(spanStart, spanEnd);
    const candidate = raw.trim();
    const candidateStart = spanStart + raw.length - raw.trimStart().length;
    if (candidate.length >= 8 && candidate.length <= 160) {
      return { textSnapshot: candidate, startIntraUtf16: candidateStart, endIntraUtf16: candidateStart + candidate.length };
    }
  }
  return null;
}

function parsePreviewBook(bookId: string): ChapterContent[] {
  const text = (readContent()[bookId] ?? "").replace(/\r\n?/g, "\n");
  const blocks: Array<{ text: string; start: number; end: number }> = [];
  let current: { text: string; start: number; end: number } | null = null;
  let cursor = 0;
  for (const segment of text.match(/[^\n]*\n|[^\n]+$/g) ?? []) {
    const line = segment.endsWith("\n") ? segment.slice(0, -1) : segment;
    if (!line.trim()) {
      if (current) blocks.push(current);
      current = null;
    } else {
      if (!current) current = { text: line, start: cursor, end: cursor + line.length };
      else { current.text += `\n${line}`; current.end = cursor + line.length; }
    }
    cursor += segment.length;
  }
  if (current) blocks.push(current);
  const headingPattern = /^(?:第[零〇一二三四五六七八九十百千万两0-9]+[章节回卷](?:\s*[-—:：、.．]?\s*.*)?|chapter\s+\d+(?:\s+.*)?)$/i;
  const headingIndexes = blocks.flatMap((block, index) => block.text.trim().length <= 50 && headingPattern.test(block.text.trim()) ? [index] : []);
  const hasHeadings = headingIndexes.length >= 2 && headingIndexes.length * 2 <= blocks.length;
  const chapters: ChapterContent[] = [];
  let paragraphOrdinal = 0;
  const addChapter = (ordinal: number, title: string, start: number, end: number, body: typeof blocks) => {
    const id = `${bookId}:c:${ordinal}:v1`;
    const chapter: Chapter = { id, bookId, ordinal, title, startUtf16: start, endUtf16: end };
    const paragraphs: Paragraph[] = body.map((block) => ({
      id: `${bookId}:p:${block.start}:v1`, bookId, chapterId: id, ordinal: paragraphOrdinal++,
      text: block.text, startUtf16: block.start, endUtf16: block.end,
    }));
    chapters.push({ chapter, paragraphs });
  };
  if (!hasHeadings) {
    addChapter(0, "正文", blocks[0]?.start ?? 0, text.length, blocks);
  } else {
    const first = headingIndexes[0]!;
    if (first > 0) addChapter(0, "序章", blocks[0]!.start, blocks[first]!.start, blocks.slice(0, first));
    for (const [headingOrdinal, headingIndex] of headingIndexes.entries()) {
      const next = headingIndexes[headingOrdinal + 1] ?? blocks.length;
      const ordinal = chapters.length;
      addChapter(ordinal, blocks[headingIndex]!.text.trim(), blocks[headingIndex]!.start, next < blocks.length ? blocks[next]!.start : text.length, blocks.slice(headingIndex + 1, next));
    }
  }
  return chapters;
}

export const previewAdapter = {
  listBooks: () => readBooks(),
  getBook: (bookId: string) => readBooks().find((book) => book.id === bookId) ?? null,
  listChapters: (bookId: string) => parsePreviewBook(bookId).map((entry) => entry.chapter),
  getChapterContent: (chapterId: string): ChapterContent | null => {
    const bookId = chapterId.split(":c:")[0] ?? "";
    return parsePreviewBook(bookId).find((entry) => entry.chapter.id === chapterId) ?? null;
  },
  getReadingPosition: (bookId: string) => (readStored<Record<string, ReadingAnchor>>(positionsKey, {}))[bookId] ?? null,
  saveReadingPosition(anchor: ReadingAnchor) {
    const book = previewAdapter.getBook(anchor.bookId);
    const chapter = parsePreviewBook(anchor.bookId).find((item) => item.chapter.id === anchor.chapterId);
    const paragraph = chapter?.paragraphs.find((item) => item.id === anchor.paragraphId);
    if (!book || !chapter || !paragraph) throw new Error("阅读位置已失效，已回到本章开头。");
    const offset = Math.max(0, Math.min(paragraph.text.length, anchor.intraParagraphUtf16));
    const normalized = { ...anchor, intraParagraphUtf16: offset, absoluteUtf16: paragraph.startUtf16 + offset, nearbyText: paragraph.text.slice(0, 48), parserVersion: 1 };
    const positions = readStored<Record<string, ReadingAnchor>>(positionsKey, {});
    positions[anchor.bookId] = normalized;
    localStorage.setItem(positionsKey, JSON.stringify(positions));
    const books = readBooks();
    const storedBook = books.find((item) => item.id === anchor.bookId);
    if (storedBook) {
      storedBook.lastOpenedAt = new Date().toISOString();
      storedBook.progressRatio = Math.min(1, normalized.absoluteUtf16 / Math.max(1, storedBook.totalUtf16));
      localStorage.setItem(booksKey, JSON.stringify(books));
    }
  },
  addBookmark(bookId: string, paragraphId: string, intraUtf16: number): boolean {
    const book = previewAdapter.getBook(bookId);
    const chapter = parsePreviewBook(bookId).find((item) => item.paragraphs.some((paragraph) => paragraph.id === paragraphId));
    const paragraph = chapter?.paragraphs.find((item) => item.id === paragraphId);
    if (!book || !chapter || !paragraph) throw new Error("找不到要收藏的段落。");
    const bookmarks = readStored<Bookmark[]>(bookmarksKey, []);
    const offset = Math.max(0, Math.min(paragraph.text.length, intraUtf16));
    if (bookmarks.some((item) => item.bookId === bookId && item.paragraphId === paragraphId && item.intraUtf16 === offset)) return false;
    bookmarks.unshift({ id: newId(), bookId, bookTitle: book.title, chapterId: chapter.chapter.id, chapterTitle: chapter.chapter.title, paragraphId, intraUtf16: offset, textPreview: paragraph.text.slice(Math.max(0, offset - 16), offset + 80), createdAt: new Date().toISOString(), note: null });
    localStorage.setItem(bookmarksKey, JSON.stringify(bookmarks));
    return true;
  },
  listBookmarks(bookId?: string): Bookmark[] {
    const booksById = new Map(readBooks().map((book) => [book.id, book]));
    return readStored<Bookmark[]>(bookmarksKey, []).filter((item) => !bookId || item.bookId === bookId).map((item) => ({ ...item, bookTitle: booksById.get(item.bookId)?.title ?? item.bookTitle }));
  },
  deleteBookmark(id: string) { localStorage.setItem(bookmarksKey, JSON.stringify(readStored<Bookmark[]>(bookmarksKey, []).filter((item) => item.id !== id))); },
  addHighlight(bookId: string, startParagraphId: string, start: number, endParagraphId: string, end: number): Highlight | null {
    const book = previewAdapter.getBook(bookId);
    const chapter = parsePreviewBook(bookId).find((item) => item.paragraphs.some((paragraph) => paragraph.id === startParagraphId) && item.paragraphs.some((paragraph) => paragraph.id === endParagraphId));
    const startIndex = chapter?.paragraphs.findIndex((item) => item.id === startParagraphId) ?? -1;
    const endIndex = chapter?.paragraphs.findIndex((item) => item.id === endParagraphId) ?? -1;
    const startParagraph = chapter?.paragraphs[startIndex];
    const endParagraph = chapter?.paragraphs[endIndex];
    if (!book || !chapter || !startParagraph || !endParagraph || endIndex < startIndex) return null;
    const startOffset = Math.max(0, Math.min(startParagraph.text.length, start));
    const endOffset = Math.max(0, Math.min(endParagraph.text.length, end));
    let textSnapshot = "";
    for (let index = startIndex; index <= endIndex; index += 1) {
      const paragraph = chapter.paragraphs[index]!;
      if (index > startIndex) textSnapshot += "\n".repeat(Math.max(0, paragraph.startUtf16 - chapter.paragraphs[index - 1]!.endUtf16));
      textSnapshot += paragraph.text.slice(index === startIndex ? startOffset : 0, index === endIndex ? endOffset : paragraph.text.length);
    }
    if (endParagraph.startUtf16 + endOffset <= startParagraph.startUtf16 + startOffset || textSnapshot.length > 16_384) return null;
    const highlight: Highlight = { id: newId(), bookId, bookTitle: book.title, chapterId: chapter.chapter.id, chapterTitle: chapter.chapter.title, startParagraphId, startIntraUtf16: startOffset, endParagraphId, endIntraUtf16: endOffset, textSnapshot, createdAt: new Date().toISOString() };
    const highlights = readStored<Highlight[]>(highlightsKey, []);
    highlights.unshift(highlight);
    localStorage.setItem(highlightsKey, JSON.stringify(highlights));
    return highlight;
  },
  listHighlights(bookId?: string): Highlight[] {
    const booksById = new Map(readBooks().map((book) => [book.id, book]));
    return readStored<Highlight[]>(highlightsKey, []).filter((item) => !bookId || item.bookId === bookId).map((item) => ({ ...item, bookTitle: booksById.get(item.bookId)?.title ?? item.bookTitle }));
  },
  deleteHighlight(id: string) { localStorage.setItem(highlightsKey, JSON.stringify(readStored<Highlight[]>(highlightsKey, []).filter((item) => item.id !== id))); },
  recordParagraphView(bookId: string, paragraphId: string, dwellMs: number) {
    if (dwellMs < 1500) return;
    const views = readStored<Array<{ bookId: string; paragraphId: string; viewedAt: string; dwellMs: number }>>(viewsKey, []);
    views.push({ bookId, paragraphId, viewedAt: new Date().toISOString(), dwellMs: Math.min(300000, dwellMs) });
    localStorage.setItem(viewsKey, JSON.stringify(views));
  },
  getDailyQuote(dateKey = localDateKey()): DailyQuote | null {
    const quotes = readStored<Record<string, DailyQuote>>(quotesKey, {});
    const cached = quotes[dateKey];
    if (cached) {
      const paragraph = parsePreviewBook(cached.bookId).flatMap((entry) => entry.paragraphs).find((item) => item.id === cached.paragraphId);
      const source = paragraph?.text.slice(cached.startIntraUtf16, cached.endIntraUtf16) ?? "";
      if (source === cached.textSnapshot && cached.sourceLocalDate && ["ai", "local", "ai_fallback"].includes(cached.selectionMethod)) return { ...cached, bookTitle: previewAdapter.getBook(cached.bookId)?.title ?? cached.bookTitle };
      delete quotes[dateKey];
      localStorage.setItem(quotesKey, JSON.stringify(quotes));
    }
    const books = new Map(readBooks().map((book) => [book.id, book]));
    const eligibleViews = readStored<Array<{ bookId: string; paragraphId: string; viewedAt: string; dwellMs: number }>>(viewsKey, [])
      .map((view) => ({ ...view, readDate: localDateKey(new Date(view.viewedAt)) }))
      .filter((view) => view.readDate < dateKey);
    const sourceLocalDate = eligibleViews.reduce<string | null>((latest, view) => !latest || view.readDate > latest ? view.readDate : latest, null);
    const views = eligibleViews
      .filter((view) => view.readDate === sourceLocalDate)
      .sort((a, b) => b.dwellMs - a.dwellMs || a.paragraphId.localeCompare(b.paragraphId));
    for (const view of views) {
      const book = books.get(view.bookId);
      if (!book) continue;
      const chapterContent = parsePreviewBook(view.bookId).find((entry) => entry.paragraphs.some((paragraph) => paragraph.id === view.paragraphId));
      const paragraph = chapterContent?.paragraphs.find((item) => item.id === view.paragraphId);
      if (!paragraph) continue;
      const sentence = extractPreviewSentence(paragraph.text);
      if (!sentence) continue;
      const { textSnapshot, startIntraUtf16, endIntraUtf16 } = sentence;
      const quote: DailyQuote = {
        localDate: dateKey, bookId: book.id, bookTitle: book.title, author: book.author,
        chapterId: chapterContent!.chapter.id, chapterTitle: chapterContent!.chapter.title,
        paragraphId: paragraph.id, startIntraUtf16, endIntraUtf16, textSnapshot,
        sourceHash: (book as BookSummary & { previewHash?: string }).previewHash ?? `preview:${book.id}`,
        createdAt: new Date().toISOString(),
        sourceLocalDate: view.readDate,
        selectionMethod: "local",
      };
      quotes[dateKey] = quote;
      localStorage.setItem(quotesKey, JSON.stringify(quotes));
      return quote;
    }
    return null;
  },
  startReadingSession(bookId: string, anchor: ReadingAnchor | null): string {
    const sessions = readStored<Array<{ id: string; bookId: string; startedAt: string; firstAnchor: ReadingAnchor | null }>>(sessionsKey, []);
    const id = newId();
    sessions.push({ id, bookId, startedAt: new Date().toISOString(), firstAnchor: anchor });
    localStorage.setItem(sessionsKey, JSON.stringify(sessions));
    return id;
  },
  endReadingSession(sessionId: string, anchor: ReadingAnchor | null) {
    const sessions = readStored<Array<{ id: string; endedAt?: string; lastAnchor?: ReadingAnchor | null }>>(sessionsKey, []);
    const session = sessions.find((item) => item.id === sessionId);
    if (session) { session.endedAt = new Date().toISOString(); session.lastAnchor = anchor; localStorage.setItem(sessionsKey, JSON.stringify(sessions)); }
  },
  touchBook(bookId: string) {
    const books = readBooks();
    const book = books.find((item) => item.id === bookId);
    if (book) {
      book.lastOpenedAt = new Date().toISOString();
      localStorage.setItem(booksKey, JSON.stringify(books));
    }
  },
  updateBookMetadata(bookId: string, title: string, author: string): BookSummary {
    const normalizedTitle = title.trim();
    if (!normalizedTitle || normalizedTitle.length > 160 || author.length > 120) throw new Error("书名不能为空，且书名最多160字、作者最多120字。");
    const books = readBooks();
    const book = books.find((item) => item.id === bookId);
    if (!book) throw new Error("书籍已移出书架。");
    book.title = normalizedTitle;
    book.author = author.trim() || "未知作者";
    localStorage.setItem(booksKey, JSON.stringify(books));
    return book;
  },
  deleteBook(bookId: string): void {
    const books = readBooks().filter((book) => book.id !== bookId);
    const content = readContent();
    delete content[bookId];
    const positions = readStored<Record<string, ReadingAnchor>>(positionsKey, {});
    delete positions[bookId];
    localStorage.setItem(booksKey, JSON.stringify(books));
    localStorage.setItem(contentKey, JSON.stringify(content));
    localStorage.setItem(positionsKey, JSON.stringify(positions));
    localStorage.setItem(bookmarksKey, JSON.stringify(readStored<Bookmark[]>(bookmarksKey, []).filter((item) => item.bookId !== bookId)));
    localStorage.setItem(highlightsKey, JSON.stringify(readStored<Highlight[]>(highlightsKey, []).filter((item) => item.bookId !== bookId)));
    localStorage.setItem(viewsKey, JSON.stringify(readStored<Array<{ bookId: string; paragraphId: string }>>(viewsKey, []).filter((item) => item.bookId !== bookId)));
    const quotes = readStored<Record<string, DailyQuote>>(quotesKey, {});
    for (const [dateKey, quote] of Object.entries(quotes)) if (quote.bookId === bookId) delete quotes[dateKey];
    localStorage.setItem(quotesKey, JSON.stringify(quotes));
  },
  async importFiles(files: File[]): Promise<ImportResult[]> {
    const books = readBooks();
    const content = readContent();
    const results: ImportResult[] = [];
    for (const file of files) {
      if (!file.name.toLowerCase().endsWith(".txt")) throw new Error("仅支持 .txt 文件。");
      if (file.size > 100 * 1024 * 1024) throw new Error("文件超过 100 MiB，浏览器预览适配器不处理超大文件。");
      const text = (await file.text()).replace(/\r\n?/g, "\n");
      if (!text.trim()) throw new Error("文件为空或只有空白内容。");
      const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
      const hash = Array.from(new Uint8Array(digest), (value) => value.toString(16).padStart(2, "0")).join("");
      const previous = books.find((book) => (book as BookSummary & { previewHash?: string }).previewHash === hash);
      if (previous) {
        results.push({ bookId: previous.id, title: previous.title, chapterCount: previous.chapterCount, totalUtf16: previous.totalUtf16, status: "duplicate" });
        continue;
      }
      const id = crypto.randomUUID();
      const title = file.name.replace(/\.txt$/i, "").trim() || "未命名";
      const headingCount = (text.match(/^\s*(?:第[零〇一二三四五六七八九十百千万两0-9]+[章节回卷]|Chapter\s+\d+).{0,45}$/gim) ?? []).length;
      const summary: BookSummary & { previewHash?: string } = {
        id, title, author: "未知作者", importName: file.name,
        chapterCount: Math.max(1, headingCount), totalUtf16: text.length,
        createdAt: new Date().toISOString(), lastOpenedAt: null, progressRatio: 0,
        previewHash: hash,
      };
      books.unshift(summary);
      content[id] = text;
      results.push({ bookId: id, title, chapterCount: summary.chapterCount, totalUtf16: summary.totalUtf16, status: "imported" });
    }
    localStorage.setItem(booksKey, JSON.stringify(books));
    localStorage.setItem(contentKey, JSON.stringify(content));
    return results;
  },
};
