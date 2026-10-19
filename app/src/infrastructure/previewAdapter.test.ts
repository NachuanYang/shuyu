// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import { previewAdapter } from "./previewAdapter";

const sampleBook = {
  id: "book-1",
  title: "原书名",
  author: "未知作者",
  importName: "source.txt",
  chapterCount: 1,
  totalUtf16: 48,
  createdAt: "2026-10-08T00:00:00.000Z",
  lastOpenedAt: null,
  progressRatio: 0,
};

describe("preview bookshelf operations", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("shuyu.preview.books", JSON.stringify([sampleBook]));
    localStorage.setItem("shuyu.preview.content", JSON.stringify({ "book-1": "这是今天读过的一段真实原文，书签和摘录都必须来自这段内容。" }));
  });

  it("edits user supplied metadata and substitutes a blank author", () => {
    const updated = previewAdapter.updateBookMetadata("book-1", "  新书名  ", "  ");

    expect(updated.title).toBe("新书名");
    expect(updated.author).toBe("未知作者");
    expect(previewAdapter.listBooks()[0]?.title).toBe("新书名");
  });

  it("removes only the selected preview book and its managed copy", () => {
    previewAdapter.deleteBook("book-1");

    expect(previewAdapter.listBooks()).toEqual([]);
    expect(JSON.parse(localStorage.getItem("shuyu.preview.content") ?? "{}" )).toEqual({});
  });

  it("creates one cached daily quote only from a paragraph recorded as read", () => {
    expect(previewAdapter.getDailyQuote()).toBeNull();
    const previousRead = new Date();
    previousRead.setDate(previousRead.getDate() - 1);
    previousRead.setHours(12, 0, 0, 0);
    localStorage.setItem("shuyu.preview.views", JSON.stringify([{
      bookId: "book-1", paragraphId: "book-1:p:0:v1", viewedAt: previousRead.toISOString(), dwellMs: 2400,
    }]));

    const first = previewAdapter.getDailyQuote();
    const second = previewAdapter.getDailyQuote();

    expect(first?.textSnapshot).toBe("这是今天读过的一段真实原文，书签和摘录都必须来自这段内容。");
    expect(first?.bookTitle).toBe("原书名");
    expect(first?.sourceLocalDate).toBe(`${previousRead.getFullYear()}-${String(previousRead.getMonth() + 1).padStart(2, "0")}-${String(previousRead.getDate()).padStart(2, "0")}`);
    expect(first?.selectionMethod).toBe("local");
    expect(second).toEqual(first);
  });
});
