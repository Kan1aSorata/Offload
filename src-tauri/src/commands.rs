//! 前端可调用的命令。状态以 Rust 侧为准，前端只负责展示。

use std::path::{Path, PathBuf};

use offload_core::manifest::{self, JobRow};
use offload_core::platform::{self, VolumeInfo};
use offload_core::{Command, Config, Snapshot};
use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, State, WebviewWindow};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::{settings, tray, AppState, ONBOARDING, POPOVER, SETTINGS};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn get_state(state: State<AppState>) -> Snapshot {
    state.service.snapshot()
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Config {
    state.service.config()
}

/// 保存配置并应用副作用（开机启动），广播给所有窗口
fn apply_config(app: &AppHandle, state: &AppState, cfg: Config) -> CmdResult<Config> {
    let old = state.service.config();
    if old.launch_at_login != cfg.launch_at_login {
        let al = app.autolaunch();
        let r = if cfg.launch_at_login { al.enable() } else { al.disable() };
        if let Err(e) = r {
            tracing::warn!(error = %e, "autostart failed");
        }
    }
    settings::save(app, &cfg).map_err(err)?;
    state.service.set_config(cfg.clone());
    let _ = app.emit("settings-changed", &cfg);
    Ok(cfg)
}

#[tauri::command]
pub fn set_settings(app: AppHandle, state: State<AppState>, cfg: Config) -> CmdResult<Config> {
    apply_config(&app, &state, cfg)
}

#[derive(Serialize)]
pub struct TargetStatus {
    pub configured: bool,
    pub connected: bool,
    pub path: Option<PathBuf>,
    pub name: Option<String>,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[tauri::command]
pub fn target_status(state: State<AppState>) -> TargetStatus {
    let cfg = state.service.config();
    let connected = cfg.target_connected();
    let info = if connected { cfg.target_path.as_deref().and_then(platform::volume_info_nearest) } else { None };
    TargetStatus {
        configured: cfg.target_path.is_some(),
        connected,
        name: cfg.target_name(),
        path: cfg.target_path.clone(),
        free_bytes: info.as_ref().map(|i| i.free_bytes),
        total_bytes: info.as_ref().map(|i| i.total_bytes),
    }
}

/// 可作为备份目标的外置卷（排除相机卡）
#[tauri::command]
pub fn list_volumes() -> Vec<VolumeInfo> {
    platform::list_external_volumes().into_iter().filter(|v| !v.is_card).collect()
}

/// 把目标设为某个目录，并记下所在卷的 UUID
fn with_target(mut cfg: Config, path: PathBuf) -> Config {
    cfg.target_volume_uuid = platform::volume_info_nearest(&path).and_then(|v| v.uuid);
    cfg.target_path = Some(path);
    cfg
}

#[tauri::command]
pub async fn pick_target_folder(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<Config>> {
    let cfg = state.service.config();
    let mut dialog = app.dialog().file().set_title("选择备份目标文件夹");
    if let Some(p) = cfg.target_path.as_deref().filter(|p| p.exists()) {
        dialog = dialog.set_directory(p);
    } else {
        dialog = dialog.set_directory("/Volumes");
    }
    let Some(picked) = dialog.blocking_pick_folder() else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(err)?;
    apply_config(&app, &state, with_target(cfg, path)).map(Some)
}

/// 选择一块外置盘作为目标：备份到 `<卷>/Cards`
#[tauri::command]
pub fn use_volume(app: AppHandle, state: State<AppState>, mount: String) -> CmdResult<Config> {
    let path = Path::new(&mount).join("Cards");
    apply_config(&app, &state, with_target(state.service.config(), path))
}

#[tauri::command]
pub fn pause(state: State<AppState>) {
    state.service.pause();
}

#[tauri::command]
pub fn resume(state: State<AppState>) {
    state.service.resume();
}

#[tauri::command]
pub fn retry(state: State<AppState>) {
    state.service.send(Command::Retry);
}

#[tauri::command]
pub fn dismiss(state: State<AppState>) {
    state.service.send(Command::Dismiss);
}

/// 在访达中显示本次备份的目录
#[tauri::command]
pub fn reveal_dest(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    let snap = state.service.snapshot();
    let dir = snap.dest_dir.or_else(|| state.service.config().target_path).ok_or("尚未设置备份目标")?;
    tray::hide_popover(&app);
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(err)
}

pub fn open_target_dir(app: &AppHandle) {
    let state = app.state::<AppState>();
    match state.service.config().target_path {
        Some(p) if p.exists() => {
            let _ = app.opener().open_path(p.to_string_lossy(), None::<&str>);
        }
        _ => show_window(app, SETTINGS),
    }
}

#[tauri::command]
pub fn open_backup_dir(app: AppHandle) {
    tray::hide_popover(&app);
    open_target_dir(&app);
}

pub fn show_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.center();
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        let _ = app.emit_to(label, "window-shown", ());
    }
}

#[tauri::command]
pub fn open_settings(app: AppHandle, page: Option<String>) {
    tray::hide_popover(&app);
    show_window(&app, SETTINGS);
    if let Some(p) = page {
        let _ = app.emit_to(SETTINGS, "settings-page", p);
    }
}

#[tauri::command]
pub fn show_onboarding(app: AppHandle) {
    show_window(&app, ONBOARDING);
}

/// 引导最后一步：保存并尝试访问目标（触发「可移除宗卷」权限弹窗）
#[tauri::command]
pub fn complete_onboarding(app: AppHandle, state: State<AppState>) -> CmdResult<Config> {
    let mut cfg = state.service.config();
    cfg.onboarded = true;
    if let Some(p) = &cfg.target_path {
        let _ = std::fs::create_dir_all(p);
        let _ = std::fs::read_dir(p);
    }
    let _ = std::fs::read_dir("/Volumes").map(|rd| rd.flatten().for_each(|e| drop(std::fs::read_dir(e.path()))));
    let cfg = apply_config(&app, &state, cfg)?;
    if let Some(w) = app.get_webview_window(ONBOARDING) {
        let _ = w.hide();
    }
    tray::show_popover(&app);
    Ok(cfg)
}

#[tauri::command]
pub fn recent_jobs(state: State<AppState>, limit: Option<u32>) -> Vec<JobRow> {
    state.service.recent_jobs(limit.unwrap_or(20))
}

/// 弹窗高度跟随内容
#[tauri::command]
pub fn fit_popover(window: WebviewWindow, height: f64) -> CmdResult<()> {
    if window.label() != POPOVER {
        return Ok(());
    }
    let h = height.clamp(160.0, 720.0);
    window.set_size(LogicalSize::new(360.0, h)).map_err(err)
}

#[tauri::command]
pub fn hide_popover(app: AppHandle) {
    tray::hide_popover(&app);
}

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub manifest_path: PathBuf,
    pub files_recorded: u64,
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    let manifest_path = manifest::default_path();
    let files_recorded = manifest::Manifest::open(&manifest_path).and_then(|m| m.file_count()).unwrap_or(0);
    AppInfo { version: app.package_info().version.to_string(), manifest_path, files_recorded }
}

#[tauri::command]
pub fn quit(app: AppHandle) {
    app.exit(0);
}
