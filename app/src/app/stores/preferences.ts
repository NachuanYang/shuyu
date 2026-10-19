import { defineStore } from "pinia";
import { ref } from "vue";
import { defaultPreferences, normalizePreferences, type ReaderPreferences } from "../../domain/preferences";
import { platform } from "../../infrastructure/platform";

export const usePreferencesStore = defineStore("preferences", () => {
  const value = ref<ReaderPreferences>({ ...defaultPreferences });
  const loaded = ref(false);
  const error = ref("");

  async function load() {
    try {
      value.value = normalizePreferences(await platform.invoke<unknown>("get_preferences"));
    } catch (cause) {
      error.value = String(cause);
      value.value = { ...defaultPreferences };
    } finally {
      loaded.value = true;
    }
  }

  async function update(patch: Partial<ReaderPreferences>) {
    const next = normalizePreferences({ ...value.value, ...patch });
    value.value = next;
    try {
      value.value = normalizePreferences(await platform.invoke<unknown>("save_preferences", { preferences: next }));
      error.value = "";
    } catch (cause) {
      error.value = String(cause);
    }
  }

  return { value, loaded, error, load, update };
});
