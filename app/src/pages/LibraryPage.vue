<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { ArrowRight, BookOpen, Check, ChevronDown, FileUp, Pencil, Search, Trash2, X } from "lucide-vue-next";
import type { BookSummary, ImportFinishedEvent, ImportProgressEvent } from "../domain/models";
import { platform } from "../infrastructure/platform";

const books = ref<BookSummary[]>([]);
const search = ref("");
const sort = ref<"recent" | "created" | "title">("recent");
const sortMenuOpen = ref(false);
const sortMenuRoot = ref<HTMLElement>();
const sortOptions = [
  { value: "recent", label: "最近阅读" },
  { value: "created", label: "最近导入" },
  { value: "title", label: "书名 A–Z" },
] as const;
const activeSortLabel = computed(() => sortOptions.find((option) => option.value === sort.value)?.label ?? "最近阅读");
const errorMessage = ref("");
const notice = ref("");
const loading = ref(false);
const progress = ref<ImportProgressEvent | null>(null);
const fileInput = ref<HTMLInputElement>();
const retryEncodingPath = ref("");
const retryEncoding = ref("gb18030");
const editingBookId = ref("");
const draftTitle = ref("");
const draftAuthor = ref("");
const savingMetadata = ref(false);
const confirmingRemovalBookId = ref("");
const removingBookId = ref("");
let removeConfirmationTimer: ReturnType<typeof window.setTimeout> | undefined;
let stopDrop: (() => void) | undefined;

const filteredBooks = computed(() => {
  const needle = search.value.trim().toLocaleLowerCase();
  if (!needle) return books.value;
  return books.value.filter((book) => `${book.title} ${book.author}`.toLocaleLowerCase().includes(needle));
});

async function refreshBooks() {
  try { books.value = await platform.invoke<BookSummary[]>("list_books", { sort: sort.value }); }
  catch (error) { errorMessage.value = `读取书架失败：${String(error)}`; }
}

async function openSortMenu() {
  sortMenuOpen.value = true;
  await nextTick();
  focusSortOption(sortOptions.findIndex((option) => option.value === sort.value));
}

function closeSortMenu(returnFocus = false) {
  sortMenuOpen.value = false;
  if (returnFocus) sortMenuRoot.value?.querySelector<HTMLButtonElement>(".sort-trigger")?.focus();
}

function focusSortOption(index: number) {
  const options = sortMenuRoot.value?.querySelectorAll<HTMLButtonElement>("[role='option']");
  if (!options?.length) return;
  options[(index + options.length) % options.length]?.focus();
}

function toggleSortMenu() {
  if (sortMenuOpen.value) closeSortMenu();
  else void openSortMenu();
}

function handleSortTriggerKeydown(event: KeyboardEvent) {
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  event.preventDefault();
  void openSortMenu();
}

function handleSortOptionKeydown(event: KeyboardEvent, index: number) {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    focusSortOption(index + 1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    focusSortOption(index - 1);
  } else if (event.key === "Home") {
    event.preventDefault();
    focusSortOption(0);
  } else if (event.key === "End") {
    event.preventDefault();
    focusSortOption(sortOptions.length - 1);
  } else if (event.key === "Escape") {
    event.preventDefault();
    closeSortMenu(true);
  }
}

function chooseSort(value: typeof sort.value) {
  sort.value = value;
  closeSortMenu(true);
}

function closeSortMenuWhenClickingOutside(event: PointerEvent) {
  if (sortMenuOpen.value && !sortMenuRoot.value?.contains(event.target as Node)) closeSortMenu();
}

function startEditing(book: BookSummary) {
  editingBookId.value = book.id;
  draftTitle.value = book.title;
  draftAuthor.value = book.author;
}

async function saveMetadata(bookId: string) {
  savingMetadata.value = true;
  errorMessage.value = "";
  try {
    await platform.invoke("update_book_metadata", { bookId, title: draftTitle.value, author: draftAuthor.value });
    editingBookId.value = "";
    await refreshBooks();
    notice.value = "书名和作者已更新。";
  } catch (error) { errorMessage.value = String(error); }
  finally { savingMetadata.value = false; }
}

function resetRemoveConfirmation() {
  if (removeConfirmationTimer !== undefined) window.clearTimeout(removeConfirmationTimer);
  removeConfirmationTimer = undefined;
  confirmingRemovalBookId.value = "";
}

async function removeBook(book: BookSummary) {
  if (removingBookId.value) return;
  errorMessage.value = "";
  notice.value = "";

  if (confirmingRemovalBookId.value !== book.id) {
    resetRemoveConfirmation();
    confirmingRemovalBookId.value = book.id;
    removeConfirmationTimer = window.setTimeout(resetRemoveConfirmation, 8000);
    return;
  }

  resetRemoveConfirmation();
  removingBookId.value = book.id;
  try {
    await platform.invoke("delete_book", { bookId: book.id });
    books.value = books.value.filter((item) => item.id !== book.id);
    await refreshBooks();
  } catch (error) { errorMessage.value = `移出书架失败：${String(error)}`; }
  finally { removingBookId.value = ""; }
}

function chooseFiles() {
  if (!platform.isNative) {
    fileInput.value?.click();
    return;
  }
  void (async () => {
    const selection = await open({ multiple: true, directory: false, filters: [{ name: "TXT 电子书", extensions: ["txt"] }] });
    if (!selection) return;
    await importPaths(Array.isArray(selection) ? selection : [selection]);
  })().catch((error) => { errorMessage.value = String(error); });
}

async function importPaths(paths: string[], encoding?: string, confirmedLargeFile = false) {
  if (!paths.length || !platform.isNative) return;
  loading.value = true;
  errorMessage.value = "";
  notice.value = "";
  progress.value = null;
  retryEncodingPath.value = "";
  let importId = "";
  let pendingLargePath = "";
  let finishedResolve: ((event: ImportFinishedEvent) => void) | undefined;
  const finished = new Promise<ImportFinishedEvent>((resolve) => { finishedResolve = resolve; });
  let stopProgress: (() => void) | undefined;
  let stopFinished: (() => void) | undefined;
  try {
    stopProgress = await listen<ImportProgressEvent>("book-import-progress", ({ payload }) => {
      if (importId && payload.importId !== importId) return;
      progress.value = payload;
      if (payload.stage === "needs-confirmation") pendingLargePath = paths[payload.fileIndex] ?? "";
      if (payload.stage === "failed" && payload.message.includes("编码")) {
        retryEncodingPath.value = paths[payload.fileIndex] ?? "";
      }
    });
    stopFinished = await listen<ImportFinishedEvent>("book-import-finished", ({ payload }) => {
      if (importId && payload.importId !== importId) return;
      finishedResolve?.(payload);
    });
    importId = await platform.invoke<string>("start_import", {
      paths,
      encoding: encoding ?? null,
      confirmedLargeFile,
    });
    const result = await finished;
    await refreshBooks();
    if (result.importedCount || result.duplicateCount) {
      notice.value = `已导入 ${result.importedCount} 本，${result.duplicateCount} 本已在书架。`;
    }
    if (result.failedCount && !retryEncodingPath.value) errorMessage.value = `${result.failedCount} 个文件没有导入，请查看进度说明后重试。`;
    if (result.cancelled) notice.value = "已取消剩余导入；已完成的书籍保留在书架中。";
    if (pendingLargePath) {
      const accepted = await confirm("这个 TXT 超过 100 MiB。继续会将它复制到书隅的本地资料夹。", { title: "确认导入大文件", kind: "warning" });
      if (accepted) await importPaths([pendingLargePath], encoding, true);
    }
  } catch (error) {
    errorMessage.value = String(error);
  } finally {
    stopProgress?.();
    stopFinished?.();
    loading.value = false;
  }
}

async function onPreviewFiles(event: Event) {
  const input = event.target as HTMLInputElement;
  const files = Array.from(input.files ?? []);
  input.value = "";
  if (!files.length) return;
  loading.value = true;
  errorMessage.value = "";
  try {
    const results = await platform.importPreviewFiles(files);
    await refreshBooks();
    const imported = results.filter((result) => result.status === "imported").length;
    const duplicates = results.length - imported;
    notice.value = `预览导入 ${imported} 本，${duplicates} 本已在书架。`;
  } catch (error) { errorMessage.value = String(error); }
  finally { loading.value = false; }
}

function onPreviewDrop(event: DragEvent) {
  if (platform.isNative) return;
  event.preventDefault();
  const files = Array.from(event.dataTransfer?.files ?? []);
  if (files.length) void onPreviewFiles({ target: { files, value: "" } } as unknown as Event);
}

function retryWithSelectedEncoding() {
  if (retryEncodingPath.value) void importPaths([retryEncodingPath.value], retryEncoding.value);
}

function coverTone(title: string) {
  const total = Array.from(title).reduce((sum, char) => sum + char.codePointAt(0)!, 0);
  return `cover-tone-${total % 5}`;
}

onMounted(async () => {
  document.addEventListener("pointerdown", closeSortMenuWhenClickingOutside);
  await refreshBooks();
  if (platform.isNative) {
    stopDrop = await getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type === "drop") void importPaths(event.payload.paths);
    });
  }
});
watch(sort, () => void refreshBooks());
onBeforeUnmount(() => {
  if (removeConfirmationTimer !== undefined) window.clearTimeout(removeConfirmationTimer);
  stopDrop?.();
  document.removeEventListener("pointerdown", closeSortMenuWhenClickingOutside);
});
</script>

<template>
  <section class="page library-page">
    <div class="library-tools">
      <label class="search-field"><Search :size="17" /><input v-model="search" type="search" aria-label="搜索书名或作者" /></label>
      <div ref="sortMenuRoot" class="sort-field sort-select">
        <button class="sort-trigger" type="button" aria-haspopup="listbox" :aria-controls="sortMenuOpen ? 'library-sort-options' : undefined" :aria-expanded="sortMenuOpen" :aria-label="`书架排序，当前：${activeSortLabel}`" @click="toggleSortMenu" @keydown="handleSortTriggerKeydown">
          <span>{{ activeSortLabel }}</span><ChevronDown :size="14" :stroke-width="2" />
        </button>
        <div v-if="sortMenuOpen" id="library-sort-options" class="sort-menu" role="listbox" aria-label="书架排序选项">
          <button v-for="(option, index) in sortOptions" :key="option.value" type="button" role="option" class="sort-option" :aria-selected="sort === option.value" :tabindex="sort === option.value ? 0 : -1" @click="chooseSort(option.value)" @keydown="handleSortOptionKeydown($event, index)">
            <span>{{ option.label }}</span><Check v-if="sort === option.value" :size="14" />
          </button>
        </div>
      </div>
      <button class="button button-primary" type="button" :disabled="loading" @click="chooseFiles"><FileUp :size="16" /> 导入 TXT</button>
      <input v-if="platform.isPreview" ref="fileInput" class="visually-hidden" type="file" accept=".txt,text/plain" multiple @change="onPreviewFiles" />
    </div>
    <div v-if="errorMessage" class="import-message import-error" role="status"><span>{{ errorMessage }}</span><button type="button" aria-label="关闭错误提示" @click="errorMessage = ''"><X :size="15" /></button></div>
    <div v-if="notice" class="import-message import-success" role="status"><Check :size="16" /><span>{{ notice }}</span><button type="button" aria-label="关闭提示" @click="notice = ''"><X :size="15" /></button></div>
    <div v-if="loading" class="import-progress card-surface" role="status">
      <span class="progress-spinner"></span>
      <div class="progress-copy"><strong>{{ progress?.fileName ?? "正在准备导入" }}</strong><small>{{ progress?.message ?? "请稍候，原文件保持不变" }}</small></div>
      <button v-if="platform.isNative && progress?.importId" class="text-button" type="button" @click="platform.invoke('cancel_import', { importId: progress.importId })">取消</button>
    </div>
    <div v-if="retryEncodingPath" class="encoding-retry card-surface">
      <div><strong>请选择文件编码</strong><small>检测不可靠时不会保存乱码内容。</small></div>
      <select v-model="retryEncoding" aria-label="文件编码"><option value="gb18030">GBK / GB18030</option><option value="utf-8">UTF-8</option><option value="utf-16le">UTF-16 LE</option><option value="utf-16be">UTF-16 BE</option></select>
      <button class="button button-secondary" type="button" @click="retryWithSelectedEncoding">重试</button>
    </div>
    <div v-if="filteredBooks.length" class="book-grid">
      <article v-for="(book, index) in filteredBooks" :key="book.id" class="book-card card-surface">
        <RouterLink :to="`/read/${book.id}`" class="book-card-link" :aria-label="`阅读《${book.title}》`">
          <div class="book-cover-card" :class="coverTone(book.title)"><span class="cover-small-label">SHUYU · {{ String(index + 1).padStart(2, '0') }}</span><strong>{{ book.title }}</strong><i></i><small>{{ book.author }}</small></div>
          <div class="book-card-copy"><strong>{{ book.title }}</strong><span>{{ book.author }}</span><small>{{ book.chapterCount }} 章 <i>·</i> {{ book.totalUtf16.toLocaleString() }} 字符</small></div>
          <span class="book-open-arrow"><ArrowRight :size="16" /></span>
        </RouterLink>
        <div class="book-card-actions">
          <button class="text-button" type="button" @click="startEditing(book)"><Pencil :size="14" /> 编辑信息</button>
          <button class="text-button danger-text" :class="{ 'removal-armed': confirmingRemovalBookId === book.id }" type="button" :disabled="Boolean(removingBookId)" :aria-pressed="confirmingRemovalBookId === book.id" @click="removeBook(book)">
            <Trash2 :size="14" /> {{ removingBookId === book.id ? "正在删除…" : confirmingRemovalBookId === book.id ? "确认删除" : "移出书架" }}
          </button>
        </div>
        <form v-if="editingBookId === book.id" class="metadata-editor" @submit.prevent="saveMetadata(book.id)">
          <label>书名<input v-model="draftTitle" maxlength="160" required /></label>
          <label>作者<input v-model="draftAuthor" maxlength="120" /></label>
          <div><button class="button button-secondary" type="button" @click="editingBookId = ''">取消</button><button class="button button-primary" type="submit" :disabled="savingMetadata">保存</button></div>
        </form>
      </article>
    </div>
    <div v-else-if="!search" class="empty-library card-surface" @dragover.prevent @drop="onPreviewDrop">
      <div class="empty-book-stack" aria-hidden="true"><span></span><span></span><span></span><BookOpen :size="28" /></div>
      <h2>书架还空着</h2>
      <button class="button button-primary empty-import-button" type="button" :disabled="loading" @click="chooseFiles"><FileUp :size="16" /> 导入第一本 TXT</button>
    </div>
    <div v-else class="empty-library card-surface"><Search :size="25" /><h2>没有找到相关书籍</h2><p>试试更短的书名或作者关键词。</p><button class="quiet-link text-button" type="button" @click="search = ''">清除搜索</button></div>
  </section>
</template>
