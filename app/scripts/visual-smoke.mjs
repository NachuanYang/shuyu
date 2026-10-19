import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { chromium } from "@playwright/test";

const baseUrl = process.env.SHUYU_PREVIEW_URL ?? "http://127.0.0.1:1420";
const evidenceDirectory = resolve("..", "docs", "evidence");
await mkdir(evidenceDirectory, { recursive: true });

const body = [];
const longParagraph = "窗外的风慢慢吹过树梢，故事仍在纸页之间向前。".repeat(9);
for (let chapter = 1; chapter <= 2; chapter += 1) {
  body.push(`第${chapter === 1 ? "一" : "二"}章 ${chapter === 1 ? "起风" : "归来"}`, "");
  for (let index = 1; index <= 5_000; index += 1) {
    body.push(`第${index}段。${longParagraph}`);
    if (index < 160) body.push("");
  }
  if (chapter === 1) body.push("");
}
const fixture = {
  id: "preview-reader-fixture",
  title: "视觉验收样书",
  author: "验收样例",
  importName: "visual-fixture.txt",
  chapterCount: 2,
  totalUtf16: body.join("\n").length,
  createdAt: "2026-10-08T02:00:00.000Z",
  lastOpenedAt: "2026-10-08T02:01:00.000Z",
  progressRatio: 0.27,
  previewHash: "sha256:visual-fixture",
};
const previousReadAt = new Date();
previousReadAt.setDate(previousReadAt.getDate() - 1);
previousReadAt.setHours(12, 0, 0, 0);
assert.ok(fixture.totalUtf16 >= 2_000_000, `long-text fixture must contain at least 2,000,000 UTF-16 characters: ${fixture.totalUtf16}`);

const sizes = [
  [360, 540], [480, 650], [768, 600], [1024, 680], [1440, 900], [1600, 540],
];
async function assertNoHorizontalOverflow(page, width, stage) {
  const dimensions = await page.evaluate(() => ({ viewport: innerWidth, document: document.documentElement.scrollWidth }));
  assert.ok(dimensions.document <= width + 1, `horizontal overflow in ${stage}: ${JSON.stringify(dimensions)}`);
}
async function assertCopyRemoved(page, phrases, stage) {
  const visibleText = await page.locator("body").innerText();
  for (const phrase of phrases) {
    assert.ok(!visibleText.includes(phrase), `${stage} still shows marked copy: ${phrase}`);
  }
}
async function assertNoPlaceholders(page, stage) {
  const placeholderFields = await page.locator("input[placeholder], textarea[placeholder]").count();
  assert.equal(placeholderFields, 0, `${stage} should not show input placeholder copy`);
}
const inputPages = await Promise.all([
  readFile(new URL("../src/pages/HomePage.vue", import.meta.url), "utf8"),
  readFile(new URL("../src/pages/LibraryPage.vue", import.meta.url), "utf8"),
  readFile(new URL("../src/pages/SettingsPage.vue", import.meta.url), "utf8"),
  readFile(new URL("../src/pages/ReaderPage.vue", import.meta.url), "utf8"),
]);
assert.ok(inputPages.every((source) => !/\bplaceholder\s*=/.test(source)), "Vue pages should not define input placeholder text");
const readerSource = await readFile(new URL("../src/pages/ReaderPage.vue", import.meta.url), "utf8");
const tauriConfig = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const closeHandlerStart = readerSource.indexOf("async function installCloseHandler()");
const closeHandlerEnd = readerSource.indexOf("\nfunction handlePageHide", closeHandlerStart);
const closeHandler = readerSource.slice(closeHandlerStart, closeHandlerEnd);
const mainCapability = JSON.parse(await readFile(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8"));
assert.match(closeHandler, /await finishSession\(\);[\s\S]*await windowHandle\.destroy\(\);/, "native window close should flush the reading session before force-destroying");
assert.ok(!closeHandler.includes("windowHandle.close()"), "close handler must not recursively request another close event");
assert.ok(mainCapability.windows.includes("main"), "force-destroy permission must be scoped to the main window");
assert.ok(mainCapability.permissions.includes("core:window:allow-destroy"), "main window capability must allow the close handler to destroy its window");
assert.equal(tauriConfig.app.windows[0].title, "书隅", "native title bar should show only the app name");
assert.match(tauriConfig.app.security.csp, /connect-src[^;]*https:\/\/zh\.wikipedia\.org/, "native Content Security Policy must allow Wikipedia article lookup");
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
  const pageErrors = [];
  const openAiRequests = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("request", (request) => {
    if (new URL(request.url()).hostname === "api.openai.com" || new URL(request.url()).hostname === "auth.openai.com") {
      openAiRequests.push(request.url());
    }
  });
  await page.addInitScript(() => {
    window.__shuyuWindowErrors = [];
    window.addEventListener("error", (event) => {
      window.__shuyuWindowErrors.push({ message: event.message, source: event.filename, line: event.lineno });
    }, true);
  });
  await page.addInitScript(({ book, text, viewedAt }) => {
    if (sessionStorage.getItem("shuyu.visualFixtureSeeded")) return;
    localStorage.setItem("shuyu.preview.books", JSON.stringify([book]));
    localStorage.setItem("shuyu.preview.content", JSON.stringify({ [book.id]: text }));
    const firstBodyOffset = text.indexOf("\n\n") + 2;
    localStorage.setItem("shuyu.preview.views", JSON.stringify([{ bookId: book.id, paragraphId: `${book.id}:p:${firstBodyOffset}:v1`, viewedAt, dwellMs: 5000 }]));
    sessionStorage.setItem("shuyu.visualFixtureSeeded", "true");
  }, { book: fixture, text: body.join("\n"), viewedAt: previousReadAt.toISOString() });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.locator(".home-book-card").first().waitFor();
  assert.equal(await page.locator(".topbar").count(), 0, "the duplicate page title and settings row should be absent");
  assert.equal(await page.locator(".home-quote.card-surface").count(), 0, "today's quote should not have a surrounding card");
  assert.ok(await page.locator(".home-quote").evaluate((quote) => Boolean(quote.compareDocumentPosition(document.querySelector(".home-reading-section")) & Node.DOCUMENT_POSITION_FOLLOWING)), "today's quote should appear before recent reading");
  assert.ok(await page.locator(".home-quote .quote-empty-text").evaluate((quote) => parseFloat(getComputedStyle(quote).textIndent) >= parseFloat(getComputedStyle(quote).fontSize) * 2 - 1), "today's quote should indent the first line by two Chinese characters");
  assert.equal(await page.locator(".home-reading-section h2").count(), 0, "the redundant continue-reading heading should be absent");
  assert.equal((await page.locator(".discovery-search button[type=submit]").innerText()).trim(), "查找", "discovery action should use the concise search label");
  await assertCopyRemoved(page, [
    "一个留给阅读的角落",
    "把时间，交还给文字。",
    "不必赶路。打开一本书，从上次停下的地方继续。",
    "回到刚才的故事",
    "书架里还有",
    "只取自你真实读过的原文",
    "AI选取 · 选自",
    "你的阅读，留在本地",
    "真实书目 · Open Library",
    "在线发现默认关闭。启用后",
    "输入书名或作者，按需检索真实书目。",
    "提交后，兴趣描述及书架书名、作者、章节数和阅读进度会发送给当前ChatGPT模型联网搜索",
    "不会发送正文、笔记、书签或摘录",
    "阅读空间",
    "继续阅读",
    "全部书籍",
    "文字在这里慢一点",
    "书隅 V1",
    "内容仅保存在这台设备",
    "探索设置",
    "偏好设置",
    "隐私设置",
  ], "home");
  assert.equal(await page.locator(".sidebar-bottom .settings-link").innerText(), "设置", "sidebar setting entry should use the unified label");
  assert.equal(await page.locator(".discovery-query-field input").getAttribute("placeholder"), null, "home discovery input should not show the marked example placeholder");
  await page.locator(".discovery-section").waitFor();
  assert.equal(await page.getByRole("textbox", { name: "描述感兴趣的作者、主题或书籍内容" }).isDisabled(), true, "discovery must be unavailable in browser preview");

  const savedViews = await page.evaluate(() => localStorage.getItem("shuyu.preview.views"));
  await page.evaluate(() => {
    localStorage.setItem("shuyu.preview.views", "[]");
    localStorage.removeItem("shuyu.preview.dailyQuotes");
  });
  await page.reload({ waitUntil: "networkidle" });
  await page.locator(".home-quote-empty").waitFor();
  await assertCopyRemoved(page, ["阅读前一日，或继续此前读过的内容后", "这里会从真实阅读的原文中选出一句摘录。"], "empty daily quote");
  await page.evaluate((views) => {
    if (views) localStorage.setItem("shuyu.preview.views", views);
    localStorage.removeItem("shuyu.preview.dailyQuotes");
  }, savedViews);
  await page.reload({ waitUntil: "networkidle" });
  await page.locator(".home-book-card").first().waitFor();

  for (const [width, height] of sizes) {
    await page.setViewportSize({ width, height });
    const suffix = `${width}x${height}`;
    await page.goto(baseUrl, { waitUntil: "networkidle" });
    assert.equal(await page.locator(".topbar").count(), 0, `duplicate top row should be absent at ${suffix}`);
    if (width <= 760) {
      await page.locator(".mobile-settings-link").waitFor({ state: "visible" });
      assert.equal(await page.locator(".mobile-settings-link").getAttribute("href"), "#/settings", `compact settings entry should follow primary icons at ${suffix}`);
      await page.mouse.move(width - 5, height - 5);
      const compactNavBoxes = await page.locator(".primary-nav .nav-link").evaluateAll((items) => items.map((item) => {
        const { x, y, width: boxWidth, height: boxHeight } = item.getBoundingClientRect();
        return { x, y, width: boxWidth, height: boxHeight };
      }));
      assert.equal(compactNavBoxes.length, 4, `compact navigation should contain three main entries and settings at ${suffix}`);
      assert.ok(compactNavBoxes.every((box) => Math.abs(box.y - compactNavBoxes[0].y) < 1 && Math.abs(box.height - compactNavBoxes[0].height) < 1 && Math.abs(box.width - compactNavBoxes[0].width) < 1), `compact navigation icons should have matching alignment and hover target sizes at ${suffix}: ${JSON.stringify(compactNavBoxes)}`);
    } else {
      assert.equal(await page.locator(".mobile-settings-link").isVisible(), false, `compact settings entry should be hidden with the side navigation at ${suffix}`);
    }
    await assertNoPlaceholders(page, `home ${suffix}`);
    await assertNoHorizontalOverflow(page, width, `home ${suffix}`);
    await page.screenshot({ path: join(evidenceDirectory, `home-${suffix}.png`), fullPage: true });

    await page.goto(`${baseUrl}/#/library`, { waitUntil: "networkidle" });
    await page.locator(".library-tools").waitFor();
    assert.equal(await page.locator(".library-page h1").count(), 0, `redundant library title should be absent at ${suffix}`);
    await assertNoPlaceholders(page, `library ${suffix}`);
    await assertCopyRemoved(page, ["阅读收藏", "在这里，收好正在读和想读的故事。", "书籍只保存在这台设备上"], `library ${suffix}`);
    assert.equal(await page.getByRole("searchbox", { name: "搜索书名或作者" }).getAttribute("placeholder"), null, `library search placeholder should be empty at ${suffix}`);
    assert.equal(await page.getByText("排序", { exact: true }).count(), 0, `redundant sort label should be absent at ${suffix}`);
    await assertNoHorizontalOverflow(page, width, `library ${suffix}`);
    await page.screenshot({ path: join(evidenceDirectory, `library-${suffix}.png`), fullPage: true });

    await page.getByRole("link", { name: "阅读《视觉验收样书》" }).click();
    await page.locator(".reader-scroller").waitFor();
    await page.getByRole("heading", { name: /第一章 起风|第二章 归来/ }).waitFor();
    await assertNoPlaceholders(page, `reader ${suffix}`);
    const paragraphCount = await page.locator(".reader-paragraph").count();
    assert.ok(paragraphCount < 60, `virtual reader rendered ${paragraphCount} paragraphs at ${suffix}`);
    await assertNoHorizontalOverflow(page, width, `reader ${suffix}`);
    await page.screenshot({ path: join(evidenceDirectory, `reader-${suffix}.png`), fullPage: true });

    await page.getByRole("button", { name: "打开章节目录" }).click();
    assert.equal(await page.locator(".reader-toc-shelf").count(), 0, "table of contents should not show the redundant return-to-shelf footer");
    assert.equal(await page.locator(".reader-toc").getByText("返回书架").count(), 0, "table of contents should not repeat the reader toolbar's return-to-shelf action");
    await page.locator(".reader-scroller").evaluate((element) => { element.scrollTop = 900; });
    await page.getByRole("button", { name: /第二章 归来/ }).click();
    await page.getByRole("heading", { name: "第二章 归来" }).waitFor();
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await page.locator(".reader-scroller").evaluate((element) => element.scrollTop), 0, "opening a chapter from the table of contents should position its text at the top");
  }

  await page.goto(`${baseUrl}/#/library`, { waitUntil: "networkidle" });
  await page.goto(`${baseUrl}/#/notes`, { waitUntil: "networkidle" });
  await page.locator(".notes-filter").waitFor();
  assert.equal(await page.locator(".notes-page h1").count(), 0, "redundant notes title should be absent");
  await assertCopyRemoved(page, [
    "留下喜欢的句子",
    "在阅读时标记的段落，会在这里留下回去的路。",
    "读到想留住的地方时，在阅读器里添加书签或选中原文摘录。",
    "去书架阅读",
  ], "empty notes");
  await page.goto(`${baseUrl}/#/library`, { waitUntil: "networkidle" });
  await page.evaluate(() => localStorage.removeItem("shuyu.preview.positions"));
  await page.goto(`${baseUrl}/#/read/${fixture.id}`, { waitUntil: "networkidle" });
  await page.getByRole("heading", { name: "第一章 起风" }).waitFor();
  await page.getByRole("button", { name: "为当前段落添加书签" }).click();
  await page.locator(".reader-paragraph").first().evaluate((paragraph) => {
    const findText = (element) => {
      const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
      let node = walker.nextNode();
      while (node && !(node.textContent ?? "").trim()) node = walker.nextNode();
      if (!node) throw new Error("reader paragraph has no text node");
      return node;
    };
    const first = findText(paragraph);
    const second = findText(document.querySelectorAll(".reader-paragraph")[1]);
    const range = document.createRange();
    range.setStart(first, 0);
    range.setEnd(second, Math.min(8, second.textContent?.length ?? 0));
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.querySelector(".reader-scroller")?.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  });
  await page.locator(".reader-selection-toolbar").waitFor();
  await page.getByRole("button", { name: "摘录", exact: true }).click();
  await page.locator(".reader-saved-highlight").first().waitFor();
  assert.equal(await page.locator(".reader-saved-highlight").count(), 2, "cross-paragraph excerpt should mark both source paragraphs");
  await page.waitForTimeout(1800);
  await page.locator(".reader-scroller").evaluate((element) => { element.scrollTop = 1800; });
  await page.waitForTimeout(550);
  const savedPosition = await page.evaluate((bookId) => JSON.parse(localStorage.getItem("shuyu.preview.positions") ?? "{}")?.[bookId], fixture.id);
  assert.ok(savedPosition?.paragraphId && savedPosition.absoluteUtf16 > 0, "reader position was not persisted");
  await page.reload({ waitUntil: "networkidle" });
  await page.getByRole("heading", { name: /第一章 起风|第二章 归来/ }).waitFor();
  const restoredScrollTop = await page.locator(".reader-scroller").evaluate((element) => element.scrollTop);
  assert.ok(restoredScrollTop > 500, `reader position did not restore after reload: ${restoredScrollTop}`);
  const paragraphAtReadingPoint = () => page.locator(".reader-scroller").evaluate((scroller) => {
    const rect = scroller.getBoundingClientRect();
    const readingLine = rect.top + 52;
    const paragraphs = Array.from(document.querySelectorAll(".reader-paragraph"));
    const intersecting = paragraphs.filter((item) => {
      const bounds = item.getBoundingClientRect();
      return bounds.top <= readingLine && bounds.bottom > readingLine;
    }).sort((left, right) => right.getBoundingClientRect().top - left.getBoundingClientRect().top);
    const paragraph = intersecting[0] ?? paragraphs.find((item) => item.getBoundingClientRect().top > readingLine);
    return paragraph
      ?.getAttribute("data-index") ?? null;
  });
  const paragraphBeforeFontChange = await paragraphAtReadingPoint();
  assert.ok(paragraphBeforeFontChange, "expected a source paragraph at the current reading point");
  const paragraphBeforeChangeText = await page.locator(`.reader-paragraph[data-index="${paragraphBeforeFontChange}"]`).textContent();
  const restoreDiagnostics = await page.evaluate((nearbyText) => {
    const scroller = document.querySelector(".reader-scroller");
    const paragraph = Array.from(document.querySelectorAll(".reader-paragraph")).find((item) => item.textContent?.includes(nearbyText));
    return { scrollTop: scroller?.scrollTop, scrollerTop: scroller?.getBoundingClientRect().top, savedParagraph: paragraph?.getAttribute("data-index"), savedTop: paragraph?.getBoundingClientRect().top, savedBottom: paragraph?.getBoundingClientRect().bottom, active: document.querySelector(".paragraph-active")?.getAttribute("data-index") };
  }, savedPosition.nearbyText);
  assert.ok(paragraphBeforeChangeText?.includes(savedPosition.nearbyText), `restored viewport should show the saved semantic paragraph: ${JSON.stringify({ savedPosition, paragraphBeforeFontChange, paragraphBeforeChangeText, restoreDiagnostics })}`);
  await page.locator("summary[aria-label='设置']").click();
  const fontSlider = page.locator(".reader-settings-popover input[type='range']").nth(0);
  const fontLayoutState = () => page.evaluate((bookId) => {
    const scroller = document.querySelector(".reader-scroller");
    const rect = scroller?.getBoundingClientRect();
    const line = (rect?.top ?? 0) + 52;
    const paragraphs = Array.from(document.querySelectorAll(".reader-paragraph"));
    const intersecting = paragraphs.filter((item) => {
      const bounds = item.getBoundingClientRect();
      return bounds.top <= line && bounds.bottom > line;
    }).sort((left, right) => right.getBoundingClientRect().top - left.getBoundingClientRect().top);
    const paragraph = intersecting[0] ?? paragraphs.find((item) => item.getBoundingClientRect().top > line);
    return { position: JSON.parse(localStorage.getItem("shuyu.preview.positions") ?? "{}")?.[bookId], scrollTop: scroller?.scrollTop, paragraph: paragraph?.getAttribute("data-index"), top: paragraph?.getBoundingClientRect().top, bottom: paragraph?.getBoundingClientRect().bottom, active: document.querySelector(".paragraph-active")?.getAttribute("data-index"), font: document.querySelector(".reader-content")?.style.fontSize };
  }, fixture.id);
  const beforeFontState = await fontLayoutState();
  await fontSlider.evaluate((input) => { input.value = "26"; input.dispatchEvent(new Event("input", { bubbles: true })); });
  await page.waitForFunction(() => document.querySelector(".reader-content")?.style.fontSize === "26px");
  await page.waitForTimeout(120);
  const paragraphAfterFontChange = await paragraphAtReadingPoint();
  const afterFontState = await fontLayoutState();
  assert.equal(paragraphAfterFontChange, paragraphBeforeFontChange, `font resizing should preserve the paragraph at the reading point: ${JSON.stringify({ paragraphBeforeFontChange, paragraphAfterFontChange, beforeFontState, afterFontState })}`);
  await fontSlider.evaluate((input) => { input.value = "18"; input.dispatchEvent(new Event("input", { bubbles: true })); });
  await page.waitForFunction(() => document.querySelector(".reader-content")?.style.fontSize === "18px");
  await page.waitForTimeout(120);
  const paragraphBeforeWindowResize = await paragraphAtReadingPoint();
  await page.setViewportSize({ width: 480, height: 650 });
  await page.waitForTimeout(180);
  const paragraphAfterWindowResize = await paragraphAtReadingPoint();
  assert.equal(paragraphAfterWindowResize, paragraphBeforeWindowResize, `window resizing should preserve the paragraph at the reading point: ${paragraphBeforeWindowResize} -> ${paragraphAfterWindowResize}`);
  await page.setViewportSize({ width: 1600, height: 540 });
  await page.waitForTimeout(120);

  await page.goto(`${baseUrl}/#/notes`, { waitUntil: "networkidle" });
  await page.locator(".note-card").first().waitFor();
  assert.equal(await page.locator(".note-card").count(), 2, "bookmark and highlight should both appear in Notes");
  await page.getByRole("link", { name: "回到原文" }).first().click();
  await page.locator(".reader-scroller").waitFor();
  await page.getByRole("heading", { name: /第一章 起风|第二章 归来/ }).waitFor();
  await page.goto(`${baseUrl}/#/`, { waitUntil: "networkidle" });
  await page.locator(".quote-filled blockquote").waitFor();
  const quoteText = await page.locator(".quote-filled blockquote").textContent();
  assert.ok(body.join("\n").includes(quoteText?.trim() ?? ""), "daily quote must exactly match a substring from read source content");

  await page.goto(`${baseUrl}/#/settings`, { waitUntil: "networkidle" });
  await page.getByRole("heading", { name: "设置", exact: true }).waitFor();
  assert.equal(await page.locator(".topbar").count(), 0, "settings should not repeat its title in a top row");
  await assertNoPlaceholders(page, "settings");
  const aiCardBox = await page.locator(".ai-settings-card").boundingBox();
  const appearanceCardBox = await page.locator(".settings-main > .settings-card:not(.ai-settings-card)").boundingBox();
  assert.ok(aiCardBox && appearanceCardBox && aiCardBox.y < appearanceCardBox.y, "ChatGPT connection and usage settings should appear before reading appearance settings");
  await assertCopyRemoved(page, [
    "按你的习惯来",
    "阅读外观和AI服务选择会保存在本机。",
    "阅读外观",
    "找到让眼睛舒服的阅读节奏。",
    "柔和暖白",
    "清透明亮",
    "低光舒读",
    "AI为可选功能，连接ChatGPT账号与允许plan usage是两项独立授权",
    "已连接且允许plan usage。账号令牌保存在Windows当前用户DPAPI加密文件中。",
    "启用AI功能",
    "允许阅读AI问答",
    "允许AI挑选今日摘录",
    "数据与用量说明",
    "保存AI设置与许可",
    "在线书籍发现",
    "阅读始终可离线",
    "断开会删除令牌",
    "偏好设置",
  ], "settings");
  assert.equal(await page.locator(".settings-aside").count(), 0, "redundant offline reading card should be removed");
  assert.equal(await page.locator(".ai-client-note").count(), 0, "AI client explanation card should be removed");
  assert.equal(await page.locator(".ai-disclosure").count(), 0, "marked AI disclosure paragraph should be removed");
  for (const theme of ["暖纸", "浅色", "深色"]) await page.getByRole("button", { name: new RegExp(theme) }).waitFor();
  assert.equal(await page.getByRole("switch").count(), 0, "AI settings should not expose per-feature switches");
  assert.equal(await page.getByRole("button", { name: "Continue with ChatGPT" }).isDisabled(), true, "browser preview must not start OAuth");
  assert.equal(await page.locator(".ai-model-row").count(), 0, "browser preview must not fabricate a model picker");
  assert.deepEqual(openAiRequests, [], "browser preview must not contact OpenAI");
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.screenshot({ path: join(evidenceDirectory, "settings-ai-discovery-1440x900.png"), fullPage: true });

  await page.goto(`${baseUrl}/#/library`, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "编辑信息" }).click();
  await page.getByRole("textbox", { name: "书名", exact: true }).fill("编辑后的验收样书");
  await page.getByRole("textbox", { name: "作者", exact: true }).fill("本地作者");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await page.locator(".book-card-copy strong").filter({ hasText: "编辑后的验收样书" }).waitFor();
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "移出书架" }).click();
  await page.getByRole("heading", { name: "书架还空着" }).waitFor();

  const importFixture = {
    name: "import-flow.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("第一章 开始\n\n这是来自文件选择器的导入验收正文。\n\n第二章 继续\n\n导入后仍可阅读。", "utf8"),
  };
  const previewFileInput = page.locator('input[type="file"]');
  await previewFileInput.setInputFiles(importFixture);
  await page.getByText("预览导入 1 本，0 本已在书架。", { exact: true }).waitFor();
  await page.getByRole("link", { name: "阅读《import-flow》" }).waitFor();
  await previewFileInput.setInputFiles(importFixture);
  await page.getByText("预览导入 0 本，1 本已在书架。", { exact: true }).waitFor();
  assert.equal(await page.locator(".book-card").count(), 1, "same-content TXT import should not create a duplicate book");
  const windowErrors = await page.evaluate(() => window.__shuyuWindowErrors);
  assert.deepEqual(pageErrors, [], `reader raised uncaught page errors: ${JSON.stringify(pageErrors)}`);
  assert.deepEqual(windowErrors, [], `reader emitted browser window errors during layout smoke: ${JSON.stringify(windowErrors)}`);
  console.log(`Visual smoke passed for ${sizes.length} viewport sizes; page errors: ${JSON.stringify(pageErrors)}; window errors: ${JSON.stringify(windowErrors)}; screenshots: ${evidenceDirectory}`);
} finally {
  await browser.close();
}
