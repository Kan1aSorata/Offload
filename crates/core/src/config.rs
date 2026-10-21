use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout::{FolderTemplate, TemplateError, DEFAULT_TEMPLATE};

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
    /// YYYY/MM/DD/<相机型号>
    DateCamera,
    /// YYYY/MM/DD/<卡名>
    DateCard,
    /// 用户自定义的 [`Config::folder_template`]
    Custom,
}

impl FolderLayout {
    /// 预设对应的模板；自定义返回 None
    pub fn preset(self) -> Option<&'static str> {
        Some(match self {
            Self::Date => DEFAULT_TEMPLATE,
            Self::Camera => "{camera}/{year}/{month}/{day}",
            Self::Card => "{card}/{year}/{month}/{day}",
            Self::DateCamera => "{year}/{month}/{day}/{camera}",
            Self::DateCard => "{year}/{month}/{day}/{card}",
            Self::Custom => return None,
        })
    }
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
    /// 自定义归档模板，仅在 `folder_layout` 为 Custom 时生效，如 `{year}/{month}/{day}/{camera}`
    pub folder_template: String,
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
            folder_template: DEFAULT_TEMPLATE.to_string(),
            backup_proxies: false,
            popover_density: PopoverDensity::Standard,
            notify_start: true,
            notify_finish: true,
            onboarded: false,
        }
    }
}

impl Config {
    /// 当前生效的归档模板文本
    pub fn folder_template_text(&self) -> &str {
        self.folder_layout.preset().unwrap_or(&self.folder_template)
    }

    /// 保存设置前检查：自定义模板必须能解析
    pub fn validate(&self) -> Result<(), TemplateError> {
        FolderTemplate::parse(self.folder_template_text()).map(|_| ())
    }

    /// 解析后的归档模板。手改 settings.json 导致模板无效时退回按日期归档。
    pub fn folder_template(&self) -> FolderTemplate {
        FolderTemplate::parse(self.folder_template_text()).unwrap_or_else(|e| {
            tracing::warn!(template = self.folder_template_text(), error = %e, "invalid folder template, using default");
            FolderTemplate::default()
        })
    }

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
        assert_eq!(c.folder_template(), FolderTemplate::default());
    }

    #[test]
    fn custom_template_only_when_selected() {
        let mut c = Config { folder_template: "{camera}/{date}".into(), ..Default::default() };
        assert_eq!(c.folder_template_text(), DEFAULT_TEMPLATE);
        c.folder_layout = FolderLayout::Custom;
        assert_eq!(c.folder_template_text(), "{camera}/{date}");
        c.folder_template = "{lens}".into();
        assert!(c.validate().is_err());
        assert_eq!(c.folder_template(), FolderTemplate::default());
        let old: Config = serde_json::from_str(r#"{"folder_layout":"date_camera"}"#).unwrap();
        assert_eq!(old.folder_template_text(), "{year}/{month}/{day}/{camera}");
    }
}
