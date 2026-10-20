use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 目标目录下的归档方式。默认 `<根>/YYYY/MM/DD/<原文件名>`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FolderLayout {
    /// YYYY/MM/DD
    #[default]
    Date,
    /// <相机型号>/YYYY/MM/DD
    Camera,
    /// <卡名>/YYYY/MM/DD
    Card,
}

/// 菜单栏弹窗的信息量
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PopoverDensity {
    Compact,
    #[default]
    Standard,
    Detailed,
}

/// 持久化到 settings.json 的用户配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 备份根目录
    pub target_path: Option<PathBuf>,
    /// 选择目录时解析出的所在卷 UUID，用于判断目标盘是否已连接
    pub target_volume_uuid: Option<String>,
    /// 全部校验通过后弹出卡
    pub auto_eject: bool,
    /// 写入后从目标盘回读校验
    pub verify: bool,
    /// 偏执模式：额外回读源卡再比对一次
    pub paranoid_verify: bool,
    pub launch_at_login: bool,
    pub folder_layout: FolderLayout,
    /// 备份代理与低码率预览文件（DJI LRF、GoPro / Insta360 LRV、Sony SUB）
    pub backup_proxies: bool,
    pub popover_density: PopoverDensity,
    /// 开始备份时发通知
    pub notify_start: bool,
    /// 完成、失败、等待目标盘时发通知
    pub notify_finish: bool,
    /// 是否已完成首次引导
    pub onboarded: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target_path: None,
            target_volume_uuid: None,
            auto_eject: true,
            verify: true,
            paranoid_verify: false,
            launch_at_login: false,
            folder_layout: FolderLayout::Date,
            backup_proxies: false,
            popover_density: PopoverDensity::Standard,
            notify_start: true,
            notify_finish: true,
            onboarded: false,
        }
    }
}

impl Config {
    /// 目标盘当前是否已连接：路径存在，且（若记录了 UUID）所在卷的 UUID 一致。
    pub fn target_connected(&self) -> bool {
        let Some(p) = &self.target_path else { return false };
        let Some(v) = crate::platform::volume_info_nearest(p) else { return false };
        match &self.target_volume_uuid {
            Some(uuid) => v.uuid.as_deref() == Some(uuid.as_str()),
            // 没记录 UUID：路径存在即可；不存在时要求其所在卷不是系统盘（外置盘未挂载时 /Volumes/X 会落到 /）
            None => p.exists() || !v.is_root,
        }
    }

    /// 目标所在卷的显示名，如 "Photos SSD"
    pub fn target_name(&self) -> Option<String> {
        let p = self.target_path.as_ref()?;
        let mut parts = p.components();
        // /Volumes/<名称>/...
        if p.starts_with("/Volumes") {
            parts.next();
            parts.next();
            if let Some(c) = parts.next() {
                return Some(c.as_os_str().to_string_lossy().into_owned());
            }
        }
        p.file_name().map(|s| s.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_spec() {
        let c = Config::default();
        assert!(c.auto_eject);
        assert!(c.verify);
        assert!(!c.paranoid_verify);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let c: Config = serde_json::from_str(r#"{"target_path":"/Volumes/Photos SSD/Cards"}"#).unwrap();
        assert!(c.auto_eject);
        assert_eq!(c.target_name().as_deref(), Some("Photos SSD"));
    }
}
