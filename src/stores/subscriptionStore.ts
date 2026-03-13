import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import type { Subscription, Bookmark } from "../lib/types";
import {
  getSubscriptions,
  subscribeToJob,
  unsubscribeFromJob,
  getBookmarks,
  addBookmark,
  removeBookmark,
} from "../lib/tauri";

const [subscriptions, setSubscriptions] = createStore<Subscription[]>([]);
const [bookmarks, setBookmarks] = createStore<Bookmark[]>([]);
const [isLoading, setIsLoading] = createSignal(false);

// Listen for mesh sync changes and refresh subscriptions
listen("mesh:table-changed", (event: any) => {
  const action = event?.payload?.action ?? "";
  if (action === "sub_add" || action === "sub_remove") {
    loadSubscriptions();
  }
});

// Full data refresh after snapshot restore
listen("mesh:data-refreshed", () => {
  loadSubscriptions();
  loadBookmarks();
});

async function loadSubscriptions() {
  try {
    const subs = await getSubscriptions();
    setSubscriptions(subs);
  } catch (err) {
    console.error("Failed to load subscriptions:", err);
  }
}

async function loadBookmarks() {
  try {
    const bms = await getBookmarks();
    setBookmarks(bms);
  } catch (err) {
    console.error("Failed to load bookmarks:", err);
  }
}

async function subscribe(jobPath: string, jobName: string) {
  try {
    await subscribeToJob(jobPath, jobName);
    await loadSubscriptions();
  } catch (err) {
    console.error("Failed to subscribe:", err);
  }
}

async function unsubscribe(jobPath: string) {
  try {
    await unsubscribeFromJob(jobPath);
    await loadSubscriptions();
  } catch (err) {
    console.error("Failed to unsubscribe:", err);
  }
}

async function addNewBookmark(path: string, displayName: string, isProjectFolder: boolean = false) {
  try {
    await addBookmark(path, displayName, isProjectFolder);
    await loadBookmarks();
  } catch (err) {
    console.error("Failed to add bookmark:", err);
  }
}

async function removeExistingBookmark(path: string) {
  try {
    await removeBookmark(path);
    await loadBookmarks();
  } catch (err) {
    console.error("Failed to remove bookmark:", err);
  }
}

async function loadAll() {
  setIsLoading(true);
  await Promise.all([loadSubscriptions(), loadBookmarks()]);
  setIsLoading(false);
}

export const subscriptionStore = {
  subscriptions,
  bookmarks,
  isLoading,
  loadAll,
  loadSubscriptions,
  loadBookmarks,
  subscribe,
  unsubscribe,
  addBookmark: addNewBookmark,
  removeBookmark: removeExistingBookmark,
};
