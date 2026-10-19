<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import { BookMarked, BookOpen, Home, LibraryBig, Settings2 } from "lucide-vue-next";
import { usePreferencesStore } from "./app/stores/preferences";
import { useAiStore } from "./app/stores/ai";
import { platform } from "./infrastructure/platform";

const route = useRoute();
const preferences = usePreferencesStore();
const aiStore = useAiStore();
const libraryCount = ref(0);
const bootError = ref("");
const themeClass = computed(() => `theme-${preferences.value.theme}`);
const readingRoute = computed(() => route.path.startsWith("/read/"));
let refreshingAiConnection = false;
const navItems = [
  { to: "/", label: "首页", icon: Home },
  { to: "/library", label: "我的书架", icon: LibraryBig },
  { to: "/notes", label: "书签与摘录", icon: BookMarked },
];

onMounted(async () => {
  await Promise.all([preferences.load(), aiStore.load()]);
  try {
    libraryCount.value = await platform.invoke<number>("get_library_count");
  } catch (error) {
    bootError.value = String(error);
  }
  window.addEventListener("focus", refreshAiConnectionOnResume);
  document.addEventListener("visibilitychange", handleVisibilityChange);
});
onBeforeUnmount(() => {
  window.removeEventListener("focus", refreshAiConnectionOnResume);
  document.removeEventListener("visibilitychange", handleVisibilityChange);
});
function handleVisibilityChange() {
  if (document.visibilityState === "visible") void refreshAiConnectionOnResume();
}
async function refreshAiConnectionOnResume() {
  if (!platform.isNative || !aiStore.loaded || aiStore.connecting || aiStore.modelLoading || refreshingAiConnection || document.visibilityState === "hidden") return;
  refreshingAiConnection = true;
  const previous = { ...aiStore.connection };
  try {
    const current = await aiStore.refreshConnection();
    if (aiStore.connecting || aiStore.modelLoading) return;
    const statusChanged = previous.connected !== current.connected
      || previous.planUsageAuthorized !== current.planUsageAuthorized;
    if (statusChanged) {
      await aiStore.load();
      if (aiStore.connecting || aiStore.modelLoading) return;
      if (aiStore.connection.connected && aiStore.connection.planUsageAuthorized) {
        try { await aiStore.refreshModels(); }
        catch { /* The store keeps the specific model-directory error for Settings. */ }
      }
    }
  } catch (error) {
    aiStore.error = String(error);
  } finally {
    refreshingAiConnection = false;
  }
}
watch(() => route.path, async () => {
  try { libraryCount.value = await platform.invoke<number>("get_library_count"); }
  catch (error) { bootError.value = String(error); }
});
</script>

<template>
  <div class="app-frame" :class="[themeClass, { 'reading-route': readingRoute }]">
    <aside class="sidebar">
      <RouterLink class="brand" to="/" aria-label="书隅首页">
        <span class="brand-mark"><BookOpen :size="21" :stroke-width="1.8" /></span>
        <span class="brand-copy"><strong>书隅</strong><small>SHUYU READING ROOM</small></span>
      </RouterLink>
      <nav class="primary-nav" aria-label="主导航">
        <RouterLink v-for="item in navItems" :key="item.to" :to="item.to" class="nav-link" :class="{ active: route.path === item.to }">
          <component :is="item.icon" :size="18" :stroke-width="1.8" />
          <span>{{ item.label }}</span>
          <span v-if="item.to === '/library' && libraryCount" class="nav-count">{{ libraryCount }}</span>
        </RouterLink>
        <RouterLink class="nav-link settings-link mobile-settings-link" to="/settings" :class="{ active: route.path === '/settings' }" aria-label="设置">
          <Settings2 :size="18" :stroke-width="1.8" /><span>设置</span>
        </RouterLink>
      </nav>
      <div class="sidebar-bottom">
        <RouterLink class="nav-link settings-link" to="/settings" :class="{ active: route.path === '/settings' }">
          <Settings2 :size="18" :stroke-width="1.8" /><span>设置</span>
        </RouterLink>
      </div>
    </aside>
    <main class="workspace">
      <div v-if="bootError" class="global-error" role="status">
        <span>本地资料库暂时无法读取。</span><button type="button" class="text-button" @click="bootError = ''">知道了</button>
      </div>
      <RouterView />
    </main>
  </div>
</template>
