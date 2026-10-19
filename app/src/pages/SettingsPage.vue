<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Check, RefreshCw, Sparkles } from "lucide-vue-next";
import { usePreferencesStore } from "../app/stores/preferences";
import { useAiStore } from "../app/stores/ai";
import type { AiSettings } from "../domain/ai";
import { platform } from "../infrastructure/platform";

const store = usePreferencesStore();
const aiStore = useAiStore();
const preferences = computed(() => store.value);
const aiForm = ref<AiSettings>({ ...aiStore.settings });
const aiNotice = ref("");
const aiNoticeError = ref(false);
const busy = ref(false);
const loggingOut = ref(false);
const themes = [
  { key: "paper" as const, label: "暖纸" },
  { key: "light" as const, label: "浅色" },
  { key: "dark" as const, label: "深色" },
];

watch(() => aiStore.loaded, (loaded) => {
  if (loaded) aiForm.value = { ...aiStore.settings };
}, { immediate: true });

function update(key: "theme" | "fontSizePx" | "lineHeight" | "contentWidthPx", value: string | number | boolean) {
  void store.update({ [key]: value } as Parameters<typeof store.update>[0]);
}

async function confirmSelectedModel() {
  if (!platform.isNative || !aiForm.value.selectedModelSlug || !aiStore.models.some((model) => model.slug === aiForm.value.selectedModelSlug)) return;
  busy.value = true;
  aiNotice.value = "";
  try {
    const selectedModelSlug = aiForm.value.selectedModelSlug;
    const selectedModel = aiStore.models.find((model) => model.slug === selectedModelSlug);
    // The form only owns the model selection. Sign-in can update the shared
    // feature and consent settings while this page stays mounted, so merge the
    // user's selection into the latest store state instead of saving a stale copy.
    await aiStore.save({ ...aiStore.settings, selectedModelSlug });
    aiForm.value = { ...aiStore.settings };
    aiNotice.value = `已确认使用${selectedModel?.displayName ?? selectedModelSlug}，AI将使用此模型。`;
    aiNoticeError.value = false;
  } catch (error) {
    aiNotice.value = String(error);
    aiNoticeError.value = true;
  } finally {
    busy.value = false;
  }
}

async function connectChatGpt() {
  if (!platform.isNative) return;
  busy.value = true;
  aiNotice.value = "";
  try {
    const status = await aiStore.connect();
    aiNotice.value = status.planUsageAuthorized
      ? (aiStore.modelError || "ChatGPT账号已连接，plan usage权限已授予。")
      : "ChatGPT身份已连接，但尚未允许plan usage；阅读不会发送AI请求。";
    aiNoticeError.value = status.planUsageAuthorized && Boolean(aiStore.modelError);
  } catch (error) {
    aiNotice.value = String(error);
    aiNoticeError.value = true;
  } finally {
    busy.value = false;
  }
}

async function cancelChatGptSignIn() {
  try {
    await aiStore.cancelSignIn();
    aiNotice.value = "已请求取消ChatGPT登录流程。";
    aiNoticeError.value = false;
  } catch (error) {
    aiNotice.value = String(error);
    aiNoticeError.value = true;
  }
}

async function refreshModels() {
  busy.value = true;
  try {
    const models = await aiStore.refreshModels();
    aiForm.value.selectedModelSlug = aiStore.settings.selectedModelSlug;
    aiNotice.value = models.length ? `已读取当前账号的${models.length}个可用模型。` : "当前账号的模型目录为空；请检查连接账号或plan usage权限。";
    aiNoticeError.value = models.length === 0;
  } catch (error) {
    aiNotice.value = String(error);
    aiNoticeError.value = true;
  } finally {
    busy.value = false;
  }
}

async function logoutChatGpt() {
  if (!platform.isNative) return;
  if (!window.confirm("退出登录？这会删除本机保存的ChatGPT令牌、注册信息和设备标识；下次登录需要重新注册。书籍与阅读记录不会删除。")) return;
  busy.value = true;
  loggingOut.value = true;
  try {
    const result = await aiStore.disconnect(true);
    aiForm.value = { ...aiStore.settings };
    aiNotice.value = result.message;
    aiNoticeError.value = false;
  } catch (error) {
    aiNotice.value = String(error);
    aiNoticeError.value = true;
  } finally {
    loggingOut.value = false;
    busy.value = false;
  }
}

</script>

<template>
  <section class="page settings-page">
    <h1>设置</h1>
    <div class="settings-layout">
      <div class="settings-main">
        <section class="settings-card card-surface privacy-card ai-settings-card">
          <div class="settings-card-heading"><h2><Sparkles :size="17" /> AI阅读功能</h2></div>
          <div class="ai-connection-row">
            <div class="ai-connection-copy">
              <strong>ChatGPT账号</strong>
            </div>
            <div class="ai-connection-actions">
              <button v-if="!aiStore.connection.connected" class="button button-primary" type="button" :disabled="!platform.isNative || busy || aiStore.connecting" @click="connectChatGpt">{{ aiStore.connecting ? "等待浏览器授权…" : "Continue with ChatGPT" }}</button>
              <button v-if="aiStore.signInPending" class="button button-secondary" type="button" @click="cancelChatGptSignIn">取消登录</button>
              <template v-if="aiStore.connection.connected">
                <button class="button button-secondary" type="button" :disabled="busy || aiStore.connecting" @click="connectChatGpt">重新连接</button>
                <button class="button button-secondary" type="button" :disabled="busy" @click="logoutChatGpt">{{ loggingOut ? "正在退出登录……" : "退出登录" }}</button>
              </template>
            </div>
          </div>
          <p v-if="aiStore.error" class="ai-inline-message error" role="status">ChatGPT设置读取失败：{{ aiStore.error }}</p>
          <div v-if="aiStore.connection.connected && !aiStore.connection.planUsageAuthorized" class="ai-inline-message error" role="status">账号身份已验证，但OAuth未授予`chatgpt.tokens.use.direct`；请重新连接并在浏览器授权页确认plan usage权限。</div>
          <div v-if="aiStore.connection.planUsageAuthorized" class="ai-model-row">
            <label class="ai-field"><span>当前账号的模型目录</span>
              <select v-model="aiForm.selectedModelSlug" :disabled="!platform.isNative || busy || aiStore.modelLoading || aiStore.models.length === 0">
                <option :value="null">选择一个可用模型</option>
                <option v-for="model in aiStore.models" :key="model.slug" :value="model.slug">{{ model.displayName }}</option>
              </select>
            </label>
            <div class="ai-model-actions">
              <button class="button button-secondary ai-refresh-models" type="button" :disabled="!platform.isNative || busy || aiStore.modelLoading" @click="refreshModels"><RefreshCw :size="14" />{{ aiStore.modelLoading ? "正在刷新…" : "刷新模型列表" }}</button>
              <button v-if="aiStore.models.length > 0" class="button button-primary ai-confirm-model" type="button" :disabled="!platform.isNative || busy || aiStore.modelLoading || !aiForm.selectedModelSlug" @click="confirmSelectedModel">确认选择</button>
            </div>
          </div>
          <p v-if="aiStore.modelError" class="ai-inline-message error" role="status">模型目录读取失败：{{ aiStore.modelError }}</p>
          <p v-else-if="aiStore.connection.planUsageAuthorized && aiStore.models.length === 0" class="ai-inline-message">当前还没有可选模型。点击“刷新模型列表”后，书隅只显示账号目录中标记为可见的模型。</p>
          <p v-if="aiNotice" class="ai-inline-message ai-notice" :class="{ error: aiNoticeError, success: !aiNoticeError }" role="status">{{ aiNotice }}</p>
          <p v-if="store.error" class="settings-error" role="status">设置保存失败：{{ store.error }}</p>
        </section>

        <section class="settings-card card-surface">
          <div class="setting-row theme-setting">
            <div class="setting-label"><strong>页面主题</strong></div>
            <div class="theme-options" role="group" aria-label="页面主题">
              <button v-for="theme in themes" :key="theme.key" type="button" class="theme-option" :class="[`swatch-${theme.key}`, { selected: preferences.theme === theme.key }]" :aria-pressed="preferences.theme === theme.key" @click="update('theme', theme.key)">
                <span class="swatch-preview"><i></i><i></i><i></i></span>
                <span class="theme-copy"><strong>{{ theme.label }}</strong></span>
                <Check v-if="preferences.theme === theme.key" class="theme-check" :size="15" />
              </button>
            </div>
          </div>
          <div class="setting-row"><div class="setting-label"><strong>正文字号</strong><small>当前 {{ preferences.fontSizePx }} px</small></div><input :value="preferences.fontSizePx" type="range" min="14" max="28" step="1" aria-label="正文字号" @input="update('fontSizePx', Number(($event.target as HTMLInputElement).value))" /></div>
          <div class="setting-row"><div class="setting-label"><strong>行间距</strong><small>当前 {{ preferences.lineHeight.toFixed(2) }}</small></div><input :value="preferences.lineHeight" type="range" min="1.4" max="2.2" step="0.05" aria-label="行间距" @input="update('lineHeight', Number(($event.target as HTMLInputElement).value))" /></div>
          <div class="setting-row"><div class="setting-label"><strong>正文宽度</strong><small>当前 {{ preferences.contentWidthPx }} px</small></div><input :value="preferences.contentWidthPx" type="range" min="540" max="860" step="10" aria-label="正文宽度" @input="update('contentWidthPx', Number(($event.target as HTMLInputElement).value))" /></div>
        </section>
      </div>
    </div>
  </section>
</template>
