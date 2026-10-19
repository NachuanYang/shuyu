import { createRouter, createWebHashHistory } from "vue-router";

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", component: () => import("../pages/HomePage.vue"), meta: { title: "首页" } },
    { path: "/library", component: () => import("../pages/LibraryPage.vue"), meta: { title: "我的书架" } },
    { path: "/read/:bookId", component: () => import("../pages/ReaderPage.vue"), meta: { title: "正在阅读" } },
    { path: "/notes", component: () => import("../pages/NotesPage.vue"), meta: { title: "书签与摘录" } },
    { path: "/settings", component: () => import("../pages/SettingsPage.vue"), meta: { title: "设置" } },
  ],
});
