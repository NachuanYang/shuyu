import { defineStore } from "pinia";
import { ref } from "vue";
import {
  AI_PRIVACY_NOTICE_VERSION,
  defaultAiSettings,
  normalizeAiSettings,
  type AiSettings,
  type ChatGptConnectionStatus,
  type ChatGptModel,
} from "../../domain/ai";
import { platform } from "../../infrastructure/platform";

export interface DisconnectResult {
  localCredentialsRemoved: boolean;
  remoteRevocationConfirmed: boolean;
  message: string;
}

export const useAiStore = defineStore("ai", () => {
  const settings = ref<AiSettings>({ ...defaultAiSettings });
  const connection = ref<ChatGptConnectionStatus>({ connected: false, planUsageAuthorized: false });
  const models = ref<ChatGptModel[]>([]);
  const loaded = ref(false);
  const connecting = ref(false);
  const signInPending = ref(false);
  const modelLoading = ref(false);
  const modelError = ref("");
  const error = ref("");
  let connectionEpoch = 0;

  async function load() {
    if (!platform.isNative) {
      settings.value = { ...defaultAiSettings };
      connection.value = { connected: false, planUsageAuthorized: false };
      models.value = [];
      loaded.value = true;
      return;
    }
    const requestEpoch = connectionEpoch;
    try {
      const [loadedSettings, loadedConnection] = await Promise.all([
        platform.invoke<unknown>("get_chatgpt_ai_settings"),
        platform.invoke<ChatGptConnectionStatus>("get_chatgpt_connection_status"),
      ]);
      if (requestEpoch === connectionEpoch && !connecting.value) {
        settings.value = normalizeAiSettings(loadedSettings);
      }
      // A resume refresh can start just before the user begins OAuth. Do not let
      // its older native status response overwrite the in-progress sign-in.
      if (requestEpoch === connectionEpoch && !connecting.value) {
        connection.value = loadedConnection;
        if (!loadedConnection.connected || !loadedConnection.planUsageAuthorized) models.value = [];
      }
      await enableFeaturesForConnectedAccount();
      error.value = "";
    } catch (cause) {
      error.value = String(cause);
    } finally {
      loaded.value = true;
    }
  }

  async function refreshConnection() {
    if (!platform.isNative) return connection.value;
    if (connecting.value) return connection.value;
    const requestEpoch = connectionEpoch;
    const current = await platform.invoke<ChatGptConnectionStatus>("get_chatgpt_connection_status");
    // The OAuth command owns connection state until its callback has completed.
    if (requestEpoch === connectionEpoch && !connecting.value) {
      connection.value = current;
      if (!current.connected || !current.planUsageAuthorized) models.value = [];
      return current;
    }
    return connection.value;
  }

  async function save(next: AiSettings, acceptPrivacyNotice = false) {
    if (!platform.isNative) throw new Error("AI设置只能在Windows客户端中保存。");
    settings.value = normalizeAiSettings(await platform.invoke<unknown>("save_chatgpt_ai_settings", {
      settings: next,
      acceptPrivacyNotice,
    }));
    error.value = "";
    return settings.value;
  }

  async function enableFeaturesForConnectedAccount() {
    if (!connection.value.connected || !connection.value.planUsageAuthorized) return;
    const settingsNeedActivation = !settings.value.enabled
      || !settings.value.allowAiQa
      || !settings.value.allowAiDailyQuote
      || settings.value.privacyNoticeVersion !== AI_PRIVACY_NOTICE_VERSION
      || !settings.value.privacyNoticeAcceptedAt;
    if (!settingsNeedActivation) return;
    await save({
      ...settings.value,
      enabled: true,
      allowAiQa: true,
      allowAiDailyQuote: true,
    }, true);
  }

  async function observeInitialSignIn(requestEpoch: number, initialConnection: ChatGptConnectionStatus, shouldStop: () => boolean) {
    while (!shouldStop() && requestEpoch === connectionEpoch && connecting.value) {
      await new Promise((resolve) => window.setTimeout(resolve, 650));
      if (shouldStop() || requestEpoch !== connectionEpoch || !connecting.value) return;
      try {
        const current = await platform.invoke<ChatGptConnectionStatus>("get_chatgpt_connection_status");
        if (shouldStop() || requestEpoch !== connectionEpoch || !connecting.value) return;
        const newlyConnected = current.connected && !initialConnection.connected;
        const newlyAuthorized = current.connected && current.planUsageAuthorized && !initialConnection.planUsageAuthorized;
        if (newlyConnected || newlyAuthorized) {
          connection.value = current;
          if (!current.planUsageAuthorized) models.value = [];
          if (current.planUsageAuthorized) {
            try {
              await enableFeaturesForConnectedAccount();
            } catch (cause) {
              error.value = String(cause);
            }
          }
          // The encrypted credentials are now present, even if the browser's
          // final redirect response is still completing in the background.
          signInPending.value = false;
          return;
        }
      } catch {
        // OAuth may still be writing the credential files; retry until the
        // command finishes or a complete connection becomes visible.
      }
    }
  }

  async function connect() {
    if (!platform.isNative) throw new Error("请在Windows客户端中连接ChatGPT账号；浏览器预览不会执行登录。");
    const initialConnection = { ...connection.value };
    const requestEpoch = ++connectionEpoch;
    connecting.value = true;
    signInPending.value = true;
    error.value = "";
    let stopPolling = false;
    const statusPolling = (!initialConnection.connected || !initialConnection.planUsageAuthorized)
      ? observeInitialSignIn(requestEpoch, initialConnection, () => stopPolling)
      : Promise.resolve();
    try {
      const connected = await platform.invoke<ChatGptConnectionStatus>("connect_chatgpt");
      if (requestEpoch === connectionEpoch) {
        connection.value = connected;
        connectionEpoch += 1;
      }
    } finally {
      stopPolling = true;
      await statusPolling;
      signInPending.value = false;
      connecting.value = false;
    }
    // OAuth has completed. Model discovery is a separate operation and must not
    // keep the UI in the "waiting for browser authorization" state.
    models.value = [];
    modelError.value = "";
    if (connection.value.planUsageAuthorized) {
      await enableFeaturesForConnectedAccount();
      try {
        await refreshModels();
      } catch (cause) {
        modelError.value = String(cause);
      }
    }
    return connection.value;
  }

  async function cancelSignIn() {
    if (!platform.isNative) return false;
    return platform.invoke<boolean>("cancel_chatgpt_sign_in");
  }

  async function refreshModels() {
    if (!platform.isNative) throw new Error("浏览器预览不会请求ChatGPT模型目录。");
    if (!connection.value.connected || !connection.value.planUsageAuthorized) {
      throw new Error("当前账号未授权ChatGPT plan usage，无法读取推理模型目录。");
    }
    modelLoading.value = true;
    modelError.value = "";
    try {
      models.value = await platform.invoke<ChatGptModel[]>("get_chatgpt_models");
      if (settings.value.selectedModelSlug && !models.value.some((model) => model.slug === settings.value.selectedModelSlug)) {
        settings.value.selectedModelSlug = null;
      }
      return models.value;
    } catch (cause) {
      models.value = [];
      modelError.value = String(cause);
      throw cause;
    } finally {
      modelLoading.value = false;
    }
  }

  async function disconnect(forget = false) {
    if (!platform.isNative) throw new Error("请在Windows客户端中管理ChatGPT连接。");
    const requestEpoch = ++connectionEpoch;
    const result = await platform.invoke<DisconnectResult>(
      forget ? "forget_chatgpt_registration" : "disconnect_chatgpt",
    );
    if (requestEpoch === connectionEpoch) {
      connection.value = { connected: false, planUsageAuthorized: false };
      connectionEpoch += 1;
    }
    models.value = [];
    modelError.value = "";
    await save({
      ...settings.value,
      enabled: false,
      selectedModelSlug: null,
      allowAiQa: false,
      allowAiDailyQuote: false,
      ...(forget ? { privacyNoticeVersion: 0, privacyNoticeAcceptedAt: null } : {}),
    });
    return result;
  }

  return {
    settings,
    connection,
    models,
    loaded,
    connecting,
    signInPending,
    modelLoading,
    modelError,
    error,
    load,
    refreshConnection,
    save,
    enableFeaturesForConnectedAccount,
    connect,
    cancelSignIn,
    refreshModels,
    disconnect,
  };
});
