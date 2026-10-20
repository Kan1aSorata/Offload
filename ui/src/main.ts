import { mount } from "svelte";
import "./styles.css";
import Popover from "./Popover.svelte";
import Settings from "./Settings.svelte";
import Onboarding from "./Onboarding.svelte";

// 三个窗口共用一个页面，按 hash 区分
const route = location.hash.replace(/^#\/?/, "") || "popover";
const views = { popover: Popover, settings: Settings, onboarding: Onboarding } as const;
const View = views[route as keyof typeof views] ?? Popover;
document.documentElement.dataset.view = route;

// 桌面应用：禁用网页右键菜单
window.addEventListener("contextmenu", (e) => e.preventDefault());

// 开发模式下在普通浏览器预览：注入模拟的 Tauri IPC
if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
  const { installMock } = await import("./lib/mock");
  installMock();
}

export default mount(View, { target: document.getElementById("app")! });
