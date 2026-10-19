export type BookSummary = {
  id: string;
  title: string;
  author: string;
  importName: string;
  chapterCount: number;
  totalUtf16: number;
  createdAt: string;
  lastOpenedAt: string | null;
  progressRatio: number;
};

export type Chapter = {
  id: string;
  bookId: string;
  ordinal: number;
  title: string;
  startUtf16: number;
  endUtf16: number;
};

export type Paragraph = {
  id: string;
  bookId: string;
  chapterId: string;
  ordinal: number;
  text: string;
  startUtf16: number;
  endUtf16: number;
};

export type ChapterContent = { chapter: Chapter; paragraphs: Paragraph[] };

export type ReadingAnchor = {
  bookId: string;
  chapterId: string;
  paragraphId: string;
  intraParagraphUtf16: number;
  absoluteUtf16: number;
  nearbyText: string;
  parserVersion: number;
};

export type Bookmark = {
  id: string;
  bookId: string;
  bookTitle: string;
  chapterId: string;
  chapterTitle: string;
  paragraphId: string;
  intraUtf16: number;
  textPreview: string;
  createdAt: string;
  note: string | null;
};

export type Highlight = {
  id: string;
  bookId: string;
  bookTitle: string;
  chapterId: string;
  chapterTitle: string;
  startParagraphId: string;
  startIntraUtf16: number;
  endParagraphId: string;
  endIntraUtf16: number;
  textSnapshot: string;
  createdAt: string;
};

export type DailyQuote = {
  localDate: string;
  bookId: string;
  bookTitle: string;
  author: string;
  chapterId: string;
  chapterTitle: string;
  paragraphId: string;
  startIntraUtf16: number;
  endIntraUtf16: number;
  textSnapshot: string;
  sourceHash: string;
  createdAt: string;
  sourceLocalDate: string;
  selectionMethod: "ai" | "local" | "ai_fallback";
};

export type ExternalBook = {
  workId: string;
  title: string;
  author: string;
  firstPublishYear: number | null;
  editionCount: number | null;
  sourceUrl: string;
};

export type BookDiscoveryResult = {
  items: ExternalBook[];
  fromCache: boolean;
  stale: boolean;
  message: string;
};

export type BookRecommendation = {
  title: string;
  author: string;
  reason: string;
};

export type BookRecommendationResult = {
  items: BookRecommendation[];
  message: string;
};

export type ImportResult = {
  bookId: string;
  title: string;
  chapterCount: number;
  totalUtf16: number;
  status: "imported" | "duplicate";
};

export type ImportProgressEvent = {
  importId: string;
  fileName: string;
  fileIndex: number;
  totalFiles: number;
  stage: string;
  message: string;
  result: ImportResult | null;
};

export type ImportFinishedEvent = {
  importId: string;
  importedCount: number;
  duplicateCount: number;
  failedCount: number;
  cancelled: boolean;
};
