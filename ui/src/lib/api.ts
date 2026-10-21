// 与 Rust 侧对应的类型与命令封装。状态以 Rust 为唯一可信源。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type JobState =
  | "idle"
  | "detected"
  | "scanning"
  | "copying"
  | "verifying"
  | "ejecting"
  | "done"
  | "failed"
  | "waiting_for_target";

/** 单个文件内部的阶段：大文件写完后的刷盘与回读校验可能持续几十秒 */
export type FileStage = "copying" | "syncing" | "verifying" | "verifying_source";

export type FailReason =
  | { kind: "no_target" }
  | { kind: "no_space"; need_bytes: number; available_bytes: number; pending_bytes: number }
  | { kind: "card_removed" }
  | { kind: "target_lost" }
  | { kind: "verify_failed"; count: number; first: string }
  | { kind: "io"; message: string };

export interface Snapshot {
  state: JobState;
  paused: boolean;
  card_name: string | null;
  card_capacity: number;
  target_name: string | null;
  dest_dir: string | null;
  current_file: string | null;
  file_stage: FileStage | null;
  file_bytes_done: number;
  file_bytes_total: number;
  files_done: number;
  files_total: number;
  files_skipped: number;
  bytes_done: number;
  bytes_total: number;
  speed_bps: number;
  avg_speed_bps: number;
  eta_secs: number | null;
  elapsed_secs: number;
  started_at: number | null;
  finished_at: number | null;
  verified_ok: boolean;
  ejected: boolean;
  eject_error: string | null;
  fail: FailReason | null;
  last_error: string | null;
  queued: number;
}

export type FolderLayout = "date" | "camera" | "card" | "date_camera" | "date_card" | "custom";
export type PopoverDensity = "compact" | "standard" | "detailed";

export interface Config {
  target_path: string | null;
  target_volume_uuid: string | null;
  auto_eject: boolean;
  verify: boolean;
  paranoid_verify: boolean;
  launch_at_login: boolean;
  folder_layout: FolderLayout;
  /** 自定义归档模板，仅在 folder_layout 为 custom 时生效 */
  folder_template: string;
  backup_proxies: boolean;
  popover_density: PopoverDensity;
  notify_start: boolean;
  notify_finish: boolean;
  onboarded: boolean;
}

export interface TargetStatus {
  configured: boolean;
  connected: boolean;
  path: string | null;
  name: string | null;
  free_bytes: number | null;
  total_bytes: number | null;
}

export interface VolumeInfo {
  mount_point: string;
  name: string;
  uuid: string | null;
  bsd: string | null;
  total_bytes: number;
  free_bytes: number;
  is_card: boolean;
  is_root: boolean;
}

export interface JobRow {
  id: number;
  volume_uuid: string;
  card_name: string | null;
  started_at: number;
  finished_at: number | null;
  status: string;
  copied: number;
  skipped: number;
  bytes: number;
  error: string | null;
  dest_dir: string | null;
}

export interface AppInfo {
  version: string;
  manifest_path: string;
  files_recorded: number;
}

export const api = {
  getState: () => invoke<Snapshot>("get_state"),
  getSettings: () => invoke<Config>("get_settings"),
  setSettings: (cfg: Config) => invoke<Config>("set_settings", { cfg }),
  targetStatus: () => invoke<TargetStatus>("target_status"),
  listVolumes: () => invoke<VolumeInfo[]>("list_volumes"),
  pickTargetFolder: () => invoke<Config | null>("pick_target_folder"),
  useVolume: (mount: string) => invoke<Config>("use_volume", { mount }),
  pause: () => invoke<void>("pause"),
  resume: () => invoke<void>("resume"),
  retry: () => invoke<void>("retry"),
  dismiss: () => invoke<void>("dismiss"),
  revealDest: () => invoke<void>("reveal_dest"),
  openBackupDir: () => invoke<void>("open_backup_dir"),
  openSettings: (page?: string) => invoke<void>("open_settings", { page: page ?? null }),
  showOnboarding: () => invoke<void>("show_onboarding"),
  completeOnboarding: () => invoke<Config>("complete_onboarding"),
  recentJobs: (limit = 20) => invoke<JobRow[]>("recent_jobs", { limit }),
  fitPopover: (height: number) => invoke<void>("fit_popover", { height }),
  hidePopover: () => invoke<void>("hide_popover"),
  appInfo: () => invoke<AppInfo>("app_info"),
  quit: () => invoke<void>("quit"),
};

export function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}

// —— 格式化 ——

/** 十进制单位，与访达一致：48.2 GB */
export function fmtBytes(n: number | null | undefined): string {
  if (n == null) return "—";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1000 && i < u.length - 1) {
    v /= 1000;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${u[i]}`;
}

/** 速度拆成数字和单位，便于大号数字排版：236 / MB/s */
export function fmtSpeed(bps: number): { value: string; unit: string } {
  if (bps >= 1e9) return { value: (bps / 1e9).toFixed(2), unit: "GB/s" };
  if (bps >= 1e6) return { value: Math.round(bps / 1e6).toString(), unit: "MB/s" };
  if (bps >= 1e3) return { value: Math.round(bps / 1e3).toString(), unit: "KB/s" };
  return { value: bps.toString(), unit: "B/s" };
}

/** 5 分 02 秒 */
export function fmtDuration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${String(r).padStart(2, "0")} 秒`;
  return `${r} 秒`;
}

/** 今天 14:32 / 昨天 09:10 / 10月18日 14:32 */
export function fmtWhen(unix: number | null | undefined): string {
  if (!unix) return "—";
  const d = new Date(unix * 1000);
  const now = new Date();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(now) - day(d)) / 86400000);
  if (diff === 0) return `今天 ${hm}`;
  if (diff === 1) return `昨天 ${hm}`;
  if (d.getFullYear() === now.getFullYear()) return `${d.getMonth() + 1}月${d.getDate()}日 ${hm}`;
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日`;
}

/** /Volumes/Photos SSD/Cards/2026/10/20 → Photos SSD/Cards/2026/10/20 */
export function shortPath(p: string | null | undefined): string {
  if (!p) return "—";
  return p.replace(/^\/Volumes\//, "");
}

export function percent(done: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(100, Math.floor((done * 100) / total));
}
