import { invoke } from "@tauri-apps/api/core";
import type { ReadingAnchor } from "../domain/models";
import { defaultPreferences, normalizePreferences } from "../domain/preferences";
import { previewAdapter } from "./previewAdapter";

const isNative = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const previewKey = "shuyu.preview.preferences";

export const platform = {
  get isNative() { return isNative; },
  get isPreview() { return !isNative; },
  importPreviewFiles: (files: File[]) => previewAdapter.importFiles(files),
  async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    if (isNative) return invoke<T>(command, args);
    if (command === "get_preferences") {
      const saved = localStorage.getItem(previewKey);
      return (saved ? normalizePreferences(JSON.parse(saved)) : { ...defaultPreferences }) as T;
    }
    if (command === "save_preferences") {
      const preferences = normalizePreferences(args?.preferences);
      localStorage.setItem(previewKey, JSON.stringify(preferences));
      return preferences as T;
    }
    if (command === "get_library_count") return previewAdapter.listBooks().length as T;
    if (command === "list_books") {
      const sort = args?.sort;
      const books = previewAdapter.listBooks().slice();
      if (sort === "title") books.sort((a, b) => a.title.localeCompare(b.title));
      else if (sort === "created") books.sort((a, b) => b.createdAt.localeCompare(a.createdAt));
      else books.sort((a, b) => (b.lastOpenedAt ?? b.createdAt).localeCompare(a.lastOpenedAt ?? a.createdAt));
      return books as T;
    }
    if (command === "get_book") return previewAdapter.getBook(String(args?.bookId ?? "")) as T;
    if (command === "list_chapters") return previewAdapter.listChapters(String(args?.bookId ?? "")) as T;
    if (command === "get_chapter_content") return previewAdapter.getChapterContent(String(args?.chapterId ?? "")) as T;
    if (command === "touch_book") { previewAdapter.touchBook(String(args?.bookId ?? "")); return undefined as T; }
    if (command === "get_reading_position") return previewAdapter.getReadingPosition(String(args?.bookId ?? "")) as T;
    if (command === "save_reading_position") { previewAdapter.saveReadingPosition(args?.anchor as ReadingAnchor); return undefined as T; }
    if (command === "add_bookmark") return previewAdapter.addBookmark(String(args?.bookId ?? ""), String(args?.paragraphId ?? ""), Number(args?.intraUtf16 ?? 0)) as T;
    if (command === "list_bookmarks") return previewAdapter.listBookmarks(args?.bookId ? String(args.bookId) : undefined) as T;
    if (command === "delete_bookmark") { previewAdapter.deleteBookmark(String(args?.bookmarkId ?? "")); return undefined as T; }
    if (command === "add_highlight") return previewAdapter.addHighlight(String(args?.bookId ?? ""), String(args?.startParagraphId ?? ""), Number(args?.startIntraUtf16 ?? 0), String(args?.endParagraphId ?? ""), Number(args?.endIntraUtf16 ?? 0)) as T;
    if (command === "list_highlights") return previewAdapter.listHighlights(args?.bookId ? String(args.bookId) : undefined) as T;
    if (command === "delete_highlight") { previewAdapter.deleteHighlight(String(args?.highlightId ?? "")); return undefined as T; }
    if (command === "record_paragraph_view") { previewAdapter.recordParagraphView(String(args?.bookId ?? ""), String(args?.paragraphId ?? ""), Number(args?.dwellMs ?? 0)); return undefined as T; }
    if (command === "get_daily_quote") return previewAdapter.getDailyQuote() as T;
    if (command === "start_reading_session") return previewAdapter.startReadingSession(String(args?.bookId ?? ""), (args?.anchor as ReadingAnchor | null) ?? null) as T;
    if (command === "end_reading_session") { previewAdapter.endReadingSession(String(args?.sessionId ?? ""), (args?.anchor as ReadingAnchor | null) ?? null); return undefined as T; }
    if (command === "update_book_metadata") return previewAdapter.updateBookMetadata(String(args?.bookId ?? ""), String(args?.title ?? ""), String(args?.author ?? "未知作者")) as T;
    if (command === "delete_book") { previewAdapter.deleteBook(String(args?.bookId ?? "")); return undefined as T; }
    throw new Error(`该操作需要 Windows 桌面环境：${command}`);
  },
};
