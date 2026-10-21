// 仅开发模式：在普通浏览器里预览界面（pnpm dev 后打开 http://localhost:1420/?state=copying#/popover）。
// 模拟 Tauri 的 IPC，状态可选 idle | scanning | copying | checking | syncing | paused | done | nonew | nospace | verify | removed | waiting | notarget
import type { Config, JobRow, Snapshot, TargetStatus, VolumeInfo } from "./api";

type Cb = (e: { event: string; id: number; payload: unknown }) => void;

export function installMock() {
  const params = new URLSearchParams(location.search);
  const scenario = params.get("state") ?? "copying";
  const callbacks = new Map<number, Cb>();
  const listeners = new Map<string, number[]>();
  let nextId = 1;

  const now = Math.floor(Date.now() / 1000);
  const GB = 1e9;
  let config: Config = {
    target_path: scenario === "notarget" ? null : "/Volumes/Photos SSD/Cards",
    target_volume_uuid: "8D1E0066",
    auto_eject: true,
    verify: true,
    paranoid_verify: false,
    launch_at_login: false,
    folder_layout: "date",
    folder_template: "{year}/{month}/{day}",
    backup_proxies: false,
    popover_density: (params.get("density") as Config["popover_density"]) ?? "standard",
    notify_start: true,
    notify_finish: true,
    onboarded: true,
  };

  const base: Snapshot = {
    state: "idle",
    paused: false,
    card_name: "EOS_R5",
    card_capacity: 128 * GB,
    target_name: "Photos SSD",
    dest_dir: "/Volumes/Photos SSD/Cards/2026/10/20",
    current_file: "IMG_4821.CR3",
    file_stage: "copying",
    file_bytes_done: 18e6,
    file_bytes_total: 45e6,
    files_done: 212,
    files_total: 312,
    files_skipped: 0,
    bytes_done: 48.2 * GB,
    bytes_total: 71.0 * GB,
    speed_bps: 236e6,
    avg_speed_bps: 236e6,
    eta_secs: 98,
    elapsed_secs: 302,
    started_at: now - 302,
    finished_at: null,
    verified_ok: false,
    ejected: false,
    eject_error: null,
    fail: null,
    last_error: null,
    queued: 0,
  };

  const scenarios: Record<string, Partial<Snapshot>> = {
    idle: { state: "idle", card_name: null },
    scanning: { state: "scanning" },
    copying: { state: "copying" },
    checking: { state: "copying", current_file: "C0070.MP4", file_stage: "verifying", file_bytes_done: 5.1 * GB, file_bytes_total: 8.05 * GB, speed_bps: 0 },
    syncing: { state: "copying", current_file: "C0070.MP4", file_stage: "syncing", file_bytes_done: 0, file_bytes_total: 8.05 * GB, speed_bps: 0 },
    paused: { state: "copying", paused: true },
    done: { state: "done", files_done: 312, bytes_done: 71 * GB, finished_at: now, ejected: true, verified_ok: true },
    nonew: { state: "done", files_total: 0, files_done: 0, files_skipped: 312, ejected: true, finished_at: now },
    nospace: {
      state: "failed",
      files_done: 140,
      fail: { kind: "no_space", need_bytes: 12.4 * GB, available_bytes: 18.6 * GB, pending_bytes: 31 * GB },
    },
    verify: { state: "failed", files_done: 310, fail: { kind: "verify_failed", count: 2, first: "IMG_4833.CR3" } },
    removed: { state: "failed", files_done: 58, fail: { kind: "card_removed" } },
    waiting: { state: "waiting_for_target", files_done: 0 },
    notarget: { state: "idle", card_name: null },
  };
  let snap: Snapshot = { ...base, ...(scenarios[scenario] ?? {}) };

  const volumes: VolumeInfo[] = [
    { mount_point: "/Volumes/Photos SSD", name: "Photos SSD", uuid: "8D1E0066", bsd: "disk4s1", total_bytes: 2e12, free_bytes: 1.4e12, is_card: false, is_root: false },
    { mount_point: "/Volumes/Samsung T7", name: "Samsung T7", uuid: "AA", bsd: "disk5s1", total_bytes: 1e12, free_bytes: 212e9, is_card: false, is_root: false },
  ];
  const jobs: JobRow[] = [
    { id: 3, volume_uuid: "x", card_name: "EOS_R5", started_at: now - 3600, finished_at: now - 3300, status: "done", copied: 312, skipped: 0, bytes: 71e9, error: null, dest_dir: null },
    { id: 2, volume_uuid: "y", card_name: "A7IV", started_at: now - 90000, finished_at: now - 89000, status: "failed", copied: 140, skipped: 0, bytes: 31e9, error: "目标盘空间不足（还需 12.4 GB）", dest_dir: null },
    { id: 1, volume_uuid: "x", card_name: "EOS_R5", started_at: now - 200000, finished_at: now - 199000, status: "no_new_files", copied: 0, skipped: 280, bytes: 0, error: null, dest_dir: null },
  ];

  function emit(event: string, payload: unknown) {
    for (const id of listeners.get(event) ?? []) callbacks.get(id)?.({ event, id, payload });
  }

  // 备份中：模拟进度推进
  setInterval(() => {
    if (snap.state === "copying" && !snap.paused && snap.file_stage === "verifying") {
      // 回读校验：只推进单文件进度，总进度不动
      snap = { ...snap, file_bytes_done: Math.min(snap.file_bytes_total, snap.file_bytes_done + 400e6 * 0.5) };
      emit("backup-progress", snap);
    } else if (snap.state === "copying" && !snap.paused && snap.file_stage !== "syncing") {
      const step = 236e6 * 0.5;
      snap = {
        ...snap,
        bytes_done: Math.min(snap.bytes_total, snap.bytes_done + step),
        files_done: Math.min(snap.files_total, snap.files_done + 1),
        eta_secs: Math.max(0, Math.round((snap.bytes_total - snap.bytes_done) / 236e6)),
        speed_bps: 220e6 + Math.round(Math.random() * 30e6),
      };
      emit("backup-progress", snap);
    }
  }, 500);

  const target = (): TargetStatus => ({
    configured: !!config.target_path,
    connected: scenario !== "waiting" && !!config.target_path,
    path: config.target_path,
    name: config.target_path ? "Photos SSD" : null,
    free_bytes: 1.4e12,
    total_bytes: 2e12,
  });

  const handlers: Record<string, (args: Record<string, unknown>) => unknown> = {
    get_state: () => snap,
    get_settings: () => config,
    set_settings: (a) => {
      config = a.cfg as Config;
      emit("settings-changed", config);
      return config;
    },
    target_status: target,
    list_volumes: () => volumes,
    pick_target_folder: () => null,
    use_volume: (a) => (config = { ...config, target_path: `${a.mount}/Cards` }),
    pause: () => emit("backup-progress", (snap = { ...snap, paused: true })),
    resume: () => emit("backup-progress", (snap = { ...snap, paused: false })),
    retry: () => emit("backup-progress", (snap = { ...base, state: "copying" })),
    dismiss: () => emit("backup-progress", (snap = { ...base, state: "idle", card_name: null })),
    recent_jobs: () => jobs,
    app_info: () => ({ version: "0.1.0", manifest_path: "~/Library/Application Support/Offload/manifest.db", files_recorded: 1284 }),
    complete_onboarding: () => config,
    "plugin:event|listen": (a) => {
      const ev = a.event as string;
      listeners.set(ev, [...(listeners.get(ev) ?? []), a.handler as number]);
      return a.handler;
    },
    "plugin:event|unlisten": () => null,
  };

  (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
    invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
      const h = handlers[cmd];
      return h ? h(args) : null;
    },
    transformCallback: (cb: Cb) => {
      const id = nextId++;
      callbacks.set(id, cb);
      return id;
    },
    unregisterCallback: (id: number) => callbacks.delete(id),
    metadata: { currentWindow: { label: location.hash.replace(/^#\/?/, "") || "popover" }, currentWebview: { label: "main" } },
  };
  (window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: () => {},
  };

  // 预览弹窗时给一个桌面底色，模拟菜单栏下的效果
  if ((location.hash.replace(/^#\/?/, "") || "popover") === "popover") {
    document.documentElement.style.background = "var(--s3)";
    document.body.style.cssText = "background: var(--s3); padding: 40px; overflow: auto";
  }
}
