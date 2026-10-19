<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { Bookmark, BookOpen, Highlighter, Trash2 } from "lucide-vue-next";
import type { Bookmark as BookmarkItem, Highlight } from "../domain/models";
import { platform } from "../infrastructure/platform";

const bookmarks = ref<BookmarkItem[]>([]);
const highlights = ref<Highlight[]>([]);
const tab = ref<"all" | "bookmarks" | "highlights">("all");
const errorMessage = ref("");
const loading = ref(true);

const entries = computed(() => [
  ...(tab.value !== "highlights" ? bookmarks.value.map((item) => ({ kind: "bookmark" as const, item, time: item.createdAt })) : []),
  ...(tab.value !== "bookmarks" ? highlights.value.map((item) => ({ kind: "highlight" as const, item, time: item.createdAt })) : []),
].sort((a, b) => b.time.localeCompare(a.time)));

async function refresh() {
  loading.value = true;
  errorMessage.value = "";
  try {
    const [loadedBookmarks, loadedHighlights] = await Promise.all([
      platform.invoke<BookmarkItem[]>("list_bookmarks", { bookId: null }),
      platform.invoke<Highlight[]>("list_highlights", { bookId: null }),
    ]);
    bookmarks.value = loadedBookmarks;
    highlights.value = loadedHighlights;
  } catch (error) { errorMessage.value = String(error); }
  finally { loading.value = false; }
}

async function removeBookmark(id: string) {
  try { await platform.invoke("delete_bookmark", { bookmarkId: id }); await refresh(); }
  catch (error) { errorMessage.value = String(error); }
}

async function removeHighlight(id: string) {
  try { await platform.invoke("delete_highlight", { highlightId: id }); await refresh(); }
  catch (error) { errorMessage.value = String(error); }
}

function dateLabel(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "本地记录" : new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium", timeStyle: "short" }).format(date);
}

onMounted(() => void refresh());
</script>

<template>
  <section class="page notes-page">
    <div class="notes-filter" role="tablist" aria-label="收藏内容筛选">
      <button type="button" role="tab" :aria-selected="tab === 'all'" :class="{ selected: tab === 'all' }" @click="tab = 'all'">全部</button>
      <button type="button" role="tab" :aria-selected="tab === 'bookmarks'" :class="{ selected: tab === 'bookmarks' }" @click="tab = 'bookmarks'"><Bookmark :size="14" /> 书签 {{ bookmarks.length }}</button>
      <button type="button" role="tab" :aria-selected="tab === 'highlights'" :class="{ selected: tab === 'highlights' }" @click="tab = 'highlights'"><Highlighter :size="14" /> 摘录 {{ highlights.length }}</button>
    </div>
    <div v-if="errorMessage" class="import-message import-error" role="status"><span>读取记录失败：{{ errorMessage }}</span><button type="button" class="text-button" @click="refresh">重试</button></div>
    <div v-if="loading" class="notes-empty card-surface"><span class="progress-spinner"></span><p>正在读取本地记录…</p></div>
    <div v-else-if="entries.length" class="notes-list">
      <article v-for="entry in entries" :key="`${entry.kind}-${entry.item.id}`" class="note-card card-surface">
        <div class="note-card-mark"><Bookmark v-if="entry.kind === 'bookmark'" :size="17" /><Highlighter v-else :size="17" /></div>
        <div class="note-card-body">
          <div class="note-source"><strong>{{ entry.item.bookTitle }}</strong><span>·</span><span>{{ entry.item.chapterTitle }}</span><time>{{ dateLabel(entry.time) }}</time></div>
          <blockquote v-if="entry.kind === 'highlight'">{{ entry.item.textSnapshot }}</blockquote>
          <p v-else class="bookmark-preview">{{ entry.item.textPreview }}</p>
          <div class="note-card-actions">
            <RouterLink class="quiet-link" :to="{ path: `/read/${entry.item.bookId}`, query: { chapterId: entry.item.chapterId, paragraphId: entry.kind === 'bookmark' ? entry.item.paragraphId : entry.item.startParagraphId, offset: String(entry.kind === 'bookmark' ? entry.item.intraUtf16 : entry.item.startIntraUtf16) } }"><BookOpen :size="14" /> 回到原文</RouterLink>
            <button v-if="entry.kind === 'bookmark'" class="text-button danger-text" type="button" :aria-label="`删除《${entry.item.bookTitle}》的书签`" @click="removeBookmark(entry.item.id)"><Trash2 :size="14" /> 删除</button>
            <button v-else class="text-button danger-text" type="button" :aria-label="`删除《${entry.item.bookTitle}》的摘录`" @click="removeHighlight(entry.item.id)"><Trash2 :size="14" /> 删除</button>
          </div>
        </div>
      </article>
    </div>
    <div v-else class="notes-empty card-surface">
      <div class="notes-empty-icons"><span><Bookmark :size="18" /></span><span><Highlighter :size="18" /></span></div>
      <h2>{{ tab === "bookmarks" ? "还没有书签" : tab === "highlights" ? "还没有摘录" : "还没有收藏的内容" }}</h2>
    </div>
  </section>
</template>
