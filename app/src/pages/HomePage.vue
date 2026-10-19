<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { ArrowRight, BookOpen, Sparkles } from "lucide-vue-next";
import type { BookRecommendationResult, BookSummary, DailyQuote } from "../domain/models";
import { canUseAiDailyQuote } from "../domain/ai";
import { useAiStore } from "../app/stores/ai";
import { platform } from "../infrastructure/platform";

const books = ref<BookSummary[]>([]);
const dailyQuote = ref<DailyQuote | null>(null);
const loadError = ref("");
const aiStore = useAiStore();
const discoveryEnabled = computed(() => platform.isNative
  && aiStore.connection.connected
  && aiStore.connection.planUsageAuthorized
  && aiStore.settings.enabled
  && !!aiStore.settings.selectedModelSlug);
const canRetryQuoteAi = computed(() => canUseAiDailyQuote(aiStore.settings, aiStore.connection, platform.isNative)
  && !!dailyQuote.value
  && dailyQuote.value.selectionMethod !== "ai");
const quoteBusy = ref(false);
const quoteError = ref("");
const quoteUsageLimit = computed(() => /用量上限|usage_limit_exceeded/i.test(quoteError.value));
const discoveryQuery = ref("");
const discovery = ref<BookRecommendationResult | null>(null);
const discoveryError = ref("");
const discoveryBusy = ref(false);
const wikipediaUrls = ref<Record<string, string>>({});
let discoveryGeneration = 0;
let homeMounted = false;

onMounted(() => {
  homeMounted = true;
  void platform.invoke<BookSummary[]>("list_books", { sort: "recent" })
    .then((loadedBooks) => { books.value = loadedBooks; })
    .catch((error) => { loadError.value = String(error); });
  void platform.invoke<DailyQuote | null>("get_daily_quote")
    .then((quote) => {
      if (!homeMounted) return;
      dailyQuote.value = quote;
      if (!quote || !platform.isNative || !canUseAiDailyQuote(aiStore.settings, aiStore.connection, platform.isNative)) return;
      void selectAiQuote(false);
    })
    .catch((error) => { quoteError.value = String(error); });
});

onUnmounted(() => {
  homeMounted = false;
});

async function selectAiQuote(forceRefresh: boolean) {
  if (!platform.isNative || !dailyQuote.value || quoteBusy.value) return;
  quoteBusy.value = true;
  quoteError.value = "";
  try {
    const result = await platform.invoke<{ quote: DailyQuote | null; aiError: string | null }>("select_ai_daily_quote", { forceRefresh });
    if (result.quote) dailyQuote.value = result.quote;
    if (result.aiError) quoteError.value = `${result.aiError} 已保留本地原文摘录。`;
    else if (result.quote?.selectionMethod === "ai_fallback") quoteError.value = "上次AI选择未完成，已保留本地原文摘录；可手动重试。";
  } catch (error) {
    quoteError.value = String(error);
  } finally {
    quoteBusy.value = false;
  }
}

function lastReadLabel(book: BookSummary) {
  if (!book.lastOpenedAt) return "刚加入书架";
  const date = new Date(book.lastOpenedAt);
  if (Number.isNaN(date.getTime())) return "最近读过";
  return `上次阅读 ${new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric" }).format(date)}`;
}

async function retryAiQuote() {
  if (!canRetryQuoteAi.value || quoteBusy.value) return;
  await selectAiQuote(true);
  if (dailyQuote.value?.selectionMethod !== "ai" && !quoteError.value) quoteError.value = "AI没有返回有效候选；本机仍显示来自已读原文的摘录。";
}

async function openQuoteUsage() {
  try {
    await platform.invoke("open_chatgpt_usage");
  } catch (error) {
    quoteError.value = `无法打开ChatGPT用量管理页面：${String(error)}`;
  }
}

function wikipediaHref(title: string) {
  return wikipediaUrls.value[title] ?? `https://zh.wikipedia.org/w/index.php?search=${encodeURIComponent(title)}`;
}

async function openWikipediaArticle(event: MouseEvent, title: string) {
  if (!platform.isNative) return;
  event.preventDefault();
  try {
    await platform.invoke("open_wikipedia_article", { url: wikipediaHref(title) });
  } catch (error) {
    discoveryError.value = `无法打开维基百科页面：${String(error)}`;
  }
}

async function recommendBooks() {
  if (!platform.isNative || !discoveryEnabled.value || discoveryBusy.value) return;
  if (discoveryQuery.value.trim().length === 0) {
    discoveryError.value = "请输入想找的书籍主题、作者或阅读兴趣。";
    return;
  }
  discoveryBusy.value = true;
  discoveryError.value = "";
  try {
    discovery.value = await platform.invoke<BookRecommendationResult>("recommend_books", {
      query: discoveryQuery.value,
    });
    const generation = ++discoveryGeneration;
    wikipediaUrls.value = Object.fromEntries(discovery.value.items.map((book) => [
      book.title,
      `https://zh.wikipedia.org/w/index.php?search=${encodeURIComponent(book.title)}`,
    ]));
    void resolveWikipediaArticles(discovery.value.items, generation);
  } catch (error) {
    discovery.value = null;
    discoveryError.value = String(error);
  } finally {
    discoveryBusy.value = false;
  }
}

async function resolveWikipediaArticles(items: BookRecommendationResult["items"], generation: number) {
  await Promise.all(items.map(async (book) => {
    const url = new URL("https://zh.wikipedia.org/w/api.php");
    url.search = new URLSearchParams({
      action: "query",
      titles: book.title,
      redirects: "1",
      prop: "info",
      inprop: "url",
      format: "json",
      origin: "*",
    }).toString();
    try {
      const response = await fetch(url.toString());
      if (!response.ok) return;
      const data = await response.json() as { query?: { pages?: Record<string, { fullurl?: string }> } };
      const articleUrl = Object.values(data.query?.pages ?? {})[0]?.fullurl;
      if (!articleUrl || generation !== discoveryGeneration) return;
      const parsed = new URL(articleUrl);
      if (parsed.protocol !== "https:" || parsed.hostname !== "zh.wikipedia.org") return;
      wikipediaUrls.value = { ...wikipediaUrls.value, [book.title]: parsed.toString() };
    } catch {
      // Keep the usable Wikipedia search link when exact article lookup fails.
    }
  }));
}
</script>

<template>
  <section class="page home-page">
    <div v-if="!books.length" class="welcome-panel">
      <div class="welcome-copy">
        <div class="welcome-icon"><BookOpen :size="22" :stroke-width="1.7" /></div>
        <h2>让喜欢的故事，<br />在这里安静地展开。</h2>
        <RouterLink to="/library" class="button button-primary">导入一本书 <ArrowRight :size="17" /></RouterLink>
      </div>
      <div class="welcome-art" aria-hidden="true">
        <div class="art-orbit orbit-one"></div><div class="art-orbit orbit-two"></div>
        <div class="book-illustration"><div class="book-cover"><span class="cover-rule"></span><span class="cover-title">慢读<br />一页</span><span class="cover-mark">SHUYU · 01</span></div><div class="book-pages"></div></div>
        <span class="art-spark spark-a">✳</span><span class="art-spark spark-b">·</span><span class="art-caption">A QUIET PLACE<br />FOR YOUR STORIES</span>
      </div>
    </div>

    <p v-if="loadError" class="import-message import-error" role="status">暂时无法读取本地书架：{{ loadError }}</p>

    <section v-if="dailyQuote" class="home-quote quote-filled" aria-label="今日摘录">
        <div class="home-quote-heading"><Sparkles :size="16" /> 今日摘录</div>
        <blockquote class="quote-empty-text">{{ dailyQuote.textSnapshot }}</blockquote>
        <p class="quote-attribution">——《{{ dailyQuote.bookTitle }}》 · {{ dailyQuote.author }} · {{ dailyQuote.chapterTitle }}</p>
        <div class="quote-source-row"><div class="quote-source-actions"><button v-if="canRetryQuoteAi" class="quote-retry-button" type="button" :disabled="quoteBusy" @click="retryAiQuote">{{ quoteBusy ? "AI挑选中…" : "让AI重新挑选" }}</button><RouterLink :to="{ path: `/read/${dailyQuote.bookId}`, query: { chapterId: dailyQuote.chapterId, paragraphId: dailyQuote.paragraphId, offset: String(dailyQuote.startIntraUtf16) } }">回到原文 <ArrowRight :size="13" /></RouterLink></div></div>
        <p v-if="quoteError" class="quote-retry-status" role="status">{{ quoteError }}</p>
        <button v-if="quoteUsageLimit" class="quiet-link quote-usage-link" type="button" :disabled="!platform.isNative" @click="openQuoteUsage">打开ChatGPT Manage usage</button>
    </section>
    <section v-if="books.length" class="home-reading-section" aria-label="最近阅读">
      <div class="home-reading-grid">
        <RouterLink v-for="(book, index) in books.slice(0, 3)" :key="book.id" class="home-book-card card-surface" :class="{ 'home-book-primary': index === 0 }" :to="`/read/${book.id}`">
          <div class="home-book-cover" :class="`cover-tone-${Array.from(book.title).reduce((sum, char) => sum + char.codePointAt(0)!, 0) % 5}`"><span>SHUYU · {{ String(index + 1).padStart(2, '0') }}</span><strong>{{ book.title }}</strong><i></i></div>
          <div class="home-book-copy"><span class="eyebrow subdued">{{ index === 0 ? "最近阅读" : "书架藏书" }}</span><h3>{{ book.title }}</h3><p>{{ book.author }}<span>·</span>{{ lastReadLabel(book) }}</p><div class="home-progress"><i :style="{ width: `${Math.round(book.progressRatio * 100)}%` }"></i></div><small>{{ book.progressRatio > 0 ? `已读 ${Math.round(book.progressRatio * 100)}%` : "从第一页开始" }}</small></div>
          <ArrowRight class="home-book-arrow" :size="17" />
        </RouterLink>
      </div>
    </section>

    <section class="discovery-section card-surface" aria-labelledby="discovery-heading">
      <div class="section-heading discovery-heading"><h2 id="discovery-heading">发现下一本书</h2></div>
      <form class="discovery-search" @submit.prevent="recommendBooks">
        <label class="discovery-query-field"><span class="sr-only">描述感兴趣的作者、主题或书籍内容</span><input v-model="discoveryQuery" type="text" maxlength="1000" autocomplete="off" :disabled="!discoveryEnabled || !platform.isNative || discoveryBusy" /></label>
        <button class="button button-primary" type="submit" :disabled="!discoveryEnabled || !platform.isNative || discoveryBusy || !discoveryQuery.trim()">{{ discoveryBusy ? "搜索中…" : "查找" }}</button>
      </form>
      <p v-if="!platform.isNative" class="discovery-status">书籍发现仅在Windows客户端中连接服务。</p>
      <p v-else-if="!discoveryEnabled" class="discovery-status">连接ChatGPT账号并选择模型后即可推荐。</p>
      <p v-else-if="discoveryError" class="discovery-status error" role="status">{{ discoveryError }}</p>
      <p v-else-if="discovery" class="discovery-status" role="status">{{ discovery.message }}</p>
      <div v-if="discovery?.items.length" class="discovery-results">
        <a v-for="(book, index) in discovery.items" :key="`${book.title}-${book.author}-${index}`" class="discovery-book-card discovery-book-link" :href="wikipediaHref(book.title)" target="_blank" rel="noopener noreferrer" :aria-label="`在维基百科查看《${book.title}》`" @click="openWikipediaArticle($event, book.title)">
          <div class="discovery-book-mark"><BookOpen :size="17" /></div>
          <div class="discovery-book-copy"><h3>{{ book.title }}</h3><p>{{ book.author }}</p><small>{{ book.reason }}</small></div>
          <ArrowRight class="discovery-book-arrow" :size="15" />
        </a>
      </div>
      <p v-else-if="discovery && !discovery.items.length" class="discovery-empty">没有找到合适的书籍。可以补充作者、主题或内容描述后重新查找。</p>
    </section>
  </section>
</template>
