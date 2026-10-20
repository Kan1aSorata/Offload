//! 配置持久化：tauri-plugin-store → settings.json

use offload_core::Config;
use tauri::{AppHandle, Manager};
use tauri_plugin_store::StoreExt;

const FILE: &str = "settings.json";
const KEY: &str = "config";
/// 改名前（PhotoSync）的 bundle id，设置文件存在以它命名的目录下
const LEGACY_IDENTIFIER: &str = "app.photosync.desktop";

/// 改名后首次启动：复制旧设置，免得用户重新走一遍引导、重新选目标盘
pub fn migrate_legacy(app: &AppHandle) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let new = dir.join(FILE);
    let Some(old) = dir.parent().map(|p| p.join(LEGACY_IDENTIFIER).join(FILE)) else {
        return;
    };
    if new.exists() || !old.exists() {
        return;
    }
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(&old, &new)) {
        Ok(_) => tracing::info!(from = %old.display(), "已迁移 PhotoSync 的设置"),
        Err(e) => tracing::warn!(error = %e, "迁移 PhotoSync 设置失败"),
    }
}

pub fn load(app: &AppHandle) -> Config {
    let Ok(store) = app.store(FILE) else {
        return Config::default();
    };
    store.get(KEY).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

pub fn save(app: &AppHandle, cfg: &Config) -> anyhow::Result<()> {
    let store = app.store(FILE)?;
    store.set(KEY, serde_json::to_value(cfg)?);
    store.save()?;
    Ok(())
}
