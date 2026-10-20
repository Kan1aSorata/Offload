//! Tauri 层：托盘、窗口、通知、命令和事件桥接。核心逻辑都在 photosync-core。

mod commands;
mod settings;
mod tray;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use photosync_core::{manifest, JobState, Notice, Service};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_notification::NotificationExt;

pub struct AppState {
    pub service: Service,
    /// 弹窗因失焦隐藏的时刻：点击托盘图标时先失焦，避免立刻又弹出来
    pub popover_hidden_at: Mutex<Option<Instant>>,
}

pub const POPOVER: &str = "popover";
pub const SETTINGS: &str = "settings";
pub const ONBOARDING: &str = "onboarding";

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,nom_exif=error,tao=warn".into()),
        )
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_popover(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 不在 Dock 显示图标
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let cfg = settings::load(app.handle());
            let (notice_tx, notice_rx) = crossbeam_channel::unbounded::<Notice>();
            let service = Service::start(cfg.clone(), manifest::default_path(), notice_tx)?;
            app.manage(AppState { service, popover_hidden_at: Mutex::new(None) });

            tray::create(app.handle())?;
            spawn_notice_forwarder(app.handle().clone(), notice_rx);
            spawn_progress_pump(app.handle().clone());
            wire_windows(app.handle());

            if !cfg.onboarded {
                commands::show_window(app.handle(), ONBOARDING);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::get_settings,
            commands::set_settings,
            commands::target_status,
            commands::list_volumes,
            commands::pick_target_folder,
            commands::use_volume,
            commands::pause,
            commands::resume,
            commands::retry,
            commands::dismiss,
            commands::reveal_dest,
            commands::open_backup_dir,
            commands::open_settings,
            commands::show_onboarding,
            commands::complete_onboarding,
            commands::recent_jobs,
            commands::fit_popover,
            commands::hide_popover,
            commands::app_info,
            commands::quit,
        ])
        .build(tauri::generate_context!())
        .expect("error while building PhotoSync");

    app.run(|app, event| match event {
        // 窗口全部关闭时不退出：这是常驻菜单栏的应用
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        RunEvent::Exit => {
            if let Some(state) = app.try_state::<AppState>() {
                state.service.shutdown();
            }
        }
        _ => {}
    });
}

/// 核心层的通知 → 系统通知
fn spawn_notice_forwarder(app: tauri::AppHandle, rx: crossbeam_channel::Receiver<Notice>) {
    std::thread::Builder::new()
        .name("notices".into())
        .spawn(move || {
            for n in rx {
                let cfg = app.state::<AppState>().service.config();
                let enabled = match n {
                    Notice::Started { .. } => cfg.notify_start,
                    _ => cfg.notify_finish,
                };
                tracing::info!(title = %n.title(), body = %n.body(), enabled, "notice");
                if enabled {
                    if let Err(e) = app.notification().builder().title(n.title()).body(n.body()).show() {
                        tracing::warn!(error = %e, "notification failed");
                    }
                }
            }
        })
        .expect("spawn notices thread");
}

/// 以 10 Hz 采样进度：有变化才推给前端，并更新托盘标题
fn spawn_progress_pump(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("progress-pump".into())
        .spawn(move || {
            let mut last_json = String::new();
            let mut last_title: Option<String> = None;
            let mut last_status = String::new();
            loop {
                let snap = app.state::<AppState>().service.snapshot();
                let json = serde_json::to_string(&snap).unwrap_or_default();
                if json != last_json {
                    let _ = app.emit("backup-progress", &snap);
                    last_json = json;
                }
                let title = tray::title_for(&snap);
                if title != last_title {
                    tray::set_title(&app, title.as_deref());
                    last_title = title;
                }
                let status = tray::status_text(&snap);
                if status != last_status {
                    tray::set_status(&app, &status);
                    last_status = status;
                }
                let active = snap.state.is_active() || snap.state == JobState::WaitingForTarget;
                std::thread::sleep(Duration::from_millis(if active { 100 } else { 500 }));
            }
        })
        .expect("spawn progress thread");
}

fn wire_windows(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(POPOVER) {
        let handle = app.clone();
        w.on_window_event(move |e| {
            if let WindowEvent::Focused(false) = e {
                tray::hide_popover(&handle);
            }
        });
    }
    for label in [SETTINGS, ONBOARDING] {
        if let Some(w) = app.get_webview_window(label) {
            let win = w.clone();
            w.on_window_event(move |e| {
                if let WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    let _ = win.hide();
                }
            });
        }
    }
}
