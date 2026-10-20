//! 菜单栏图标：左键开关弹窗，右键菜单（当前状态、设置…、打开备份目录、退出）。

use std::time::{Duration, Instant};

use offload_core::{JobState, Snapshot};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_positioner::{Position, WindowExt};

use crate::{commands, AppState, POPOVER, SETTINGS};

const TRAY_ID: &str = "main";

/// 状态菜单项的句柄，用于更新文字
pub struct StatusItem(pub MenuItem<Wry>);

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "空闲 · 等待插卡", false, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, Some("CmdOrCtrl+,"))?;
    let open_dir = MenuItem::with_id(app, "open_dir", "打开备份目录", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Offload", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &open_dir,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    app.manage(StatusItem(status));

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/tray@2x.png"))?)
        .icon_as_template(true)
        .tooltip("Offload")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => commands::show_window(app, SETTINGS),
            "open_dir" => commands::open_target_dir(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                toggle_popover(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn toggle_popover(app: &AppHandle) {
    let Some(w) = app.get_webview_window(POPOVER) else { return };
    if w.is_visible().unwrap_or(false) {
        hide_popover(app);
        return;
    }
    // 刚因为失焦（点击托盘本身）而隐藏：这次点击的意图是关闭
    let recently = app
        .state::<AppState>()
        .popover_hidden_at
        .lock()
        .ok()
        .and_then(|g| *g)
        .is_some_and(|t| t.elapsed() < Duration::from_millis(300));
    if !recently {
        show_popover(app);
    }
}

pub fn show_popover(app: &AppHandle) {
    let Some(w) = app.get_webview_window(POPOVER) else { return };
    let _ = w.as_ref().window().move_window(Position::TrayBottomCenter);
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit_to(POPOVER, "popover-shown", ());
}

pub fn hide_popover(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(POPOVER) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
            if let Ok(mut g) = app.state::<AppState>().popover_hidden_at.lock() {
                *g = Some(Instant::now());
            }
        }
    }
}

/// 备份进行中托盘标题显示进度，如 ↑ 42%；空闲时只显示图标
pub fn title_for(s: &Snapshot) -> Option<String> {
    let pct = || s.percent();
    match s.state {
        JobState::Copying | JobState::Verifying if s.paused => Some(format!("‖ {}%", pct())),
        JobState::Copying | JobState::Verifying => Some(format!("↑ {}%", pct())),
        JobState::Scanning | JobState::Detected => Some("↑".into()),
        JobState::Ejecting => Some("⏏".into()),
        JobState::Failed | JobState::WaitingForTarget => Some("!".into()),
        _ => None,
    }
}

pub fn status_text(s: &Snapshot) -> String {
    let card = s.card_name.clone().unwrap_or_default();
    let pct = s.percent();
    match s.state {
        JobState::Idle => "空闲 · 等待插卡".into(),
        JobState::Detected | JobState::Scanning => format!("正在扫描「{card}」"),
        JobState::Copying if s.paused => format!("已暂停「{card}」 · {pct}%"),
        JobState::Copying => format!("正在备份「{card}」 · {pct}%"),
        JobState::Verifying => format!("正在校验「{card}」"),
        JobState::Ejecting => format!("正在弹出「{card}」"),
        JobState::Done => format!("已完成「{card}」"),
        JobState::Failed => format!("已停止：{}", s.last_error.clone().unwrap_or_default()),
        JobState::WaitingForTarget => "等待目标盘接入".into(),
    }
}

pub fn set_title(app: &AppHandle, title: Option<&str>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_title(title);
    }
}

pub fn set_status(app: &AppHandle, text: &str) {
    if let Some(item) = app.try_state::<StatusItem>() {
        let _ = item.0.set_text(text);
    }
}
