//! 配置持久化：tauri-plugin-store → settings.json

use photosync_core::Config;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const FILE: &str = "settings.json";
const KEY: &str = "config";

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
