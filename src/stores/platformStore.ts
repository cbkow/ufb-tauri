import { createSignal } from "solid-js";
import { getPlatform, getSpecialPaths } from "../lib/tauri";

const [platform, setPlatform] = createSignal("win");
const [homePath, setHomePath] = createSignal("");
let initialized = false;

async function init() {
  if (initialized) return;
  initialized = true;
  try {
    setPlatform(await getPlatform());
  } catch {
    const ua = navigator.platform.toLowerCase();
    if (ua.includes("linux")) setPlatform("lin");
    else if (ua.includes("mac")) setPlatform("mac");
    else setPlatform("win");
  }
  try {
    const paths = await getSpecialPaths();
    if (paths.home) setHomePath(paths.home);
  } catch {}
}

export const platformStore = {
  get platform() {
    return platform();
  },
  get home() {
    return homePath();
  },
  init,
};
