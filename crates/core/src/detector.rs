//! 卡识别：过滤系统盘、目标盘、无 DCIM 的卷；去抖；按 UUID 去重。
//!
//! 判定只用三条：有 DCIM、不是 /、不是目标盘。不按 internal/USB 属性过滤，
//! 因为 MacBook Pro 自带卡槽可能被标为内置设备。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use xxhash_rust::xxh3::xxh3_64;

use crate::config::Config;
use crate::job::{Command, Job};
use crate::platform::VolumeEvent;

/// 等 Spotlight 和文件系统稳定
pub const SETTLE: Duration = Duration::from_millis(1500);

/// 挂载事件的初步判定（不做文件 I/O）
#[derive(Debug, PartialEq, Eq)]
pub enum MountKind {
    /// 目标盘重新挂载：恢复挂起的任务
    Target,
    /// 可能是相机卡，等稳定后检查 DCIM
    MaybeCard,
    Ignore,
}

pub fn classify(uuid: Option<&str>, path: &Path, cfg: &Config) -> MountKind {
    if path == Path::new("/") {
        return MountKind::Ignore;
    }
    if let (Some(u), Some(t)) = (uuid, cfg.target_volume_uuid.as_deref()) {
        if u == t {
            return MountKind::Target;
        }
    }
    if let Some(tp) = &cfg.target_path {
        if tp.starts_with(path) {
            return MountKind::Target;
        }
    }
    MountKind::MaybeCard
}

/// 卡的唯一标识。部分 FAT32 卡拿不到卷 UUID，兜底用
/// 「卷名 + 容量 + DCIM 下第一级目录列表」的哈希。
pub fn card_identity(uuid: Option<&str>, path: &Path, name: &str, size: u64) -> String {
    if let Some(u) = uuid.filter(|u| !u.is_empty()) {
        return u.to_string();
    }
    let mut dirs: Vec<String> = std::fs::read_dir(path.join("DCIM"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    dirs.sort();
    let key = format!("{name}\0{size}\0{}", dirs.join("\0"));
    format!("fallback-{:016x}", xxh3_64(key.as_bytes()))
}

pub fn is_card(path: &Path) -> bool {
    path.exists() && path.join("DCIM").is_dir()
}

/// 消费卷事件，把相机卡变成 Job 交给 runner
pub fn run(rx: Receiver<VolumeEvent>, cfg: Arc<RwLock<Config>>, cmd: Sender<Command>) {
    // 同一个 BSD 名在稳定期内只处理一次（Appeared 与 DescriptionChanged 会各来一次）
    let pending: Arc<Mutex<HashSet<String>>> = Arc::default();
    for ev in rx {
        match ev {
            VolumeEvent::Mounted { bsd, uuid, path, name, size } => {
                let kind = classify(uuid.as_deref(), &path, &cfg.read().unwrap_or_else(|e| e.into_inner()));
                if path.starts_with("/Volumes") {
                    tracing::info!(%bsd, ?uuid, path = %path.display(), ?kind, "volume mounted");
                } else {
                    tracing::debug!(%bsd, ?uuid, path = %path.display(), ?kind, "volume mounted");
                }
                match kind {
                    MountKind::Ignore => {}
                    MountKind::Target => {
                        let _ = cmd.send(Command::TargetMounted);
                    }
                    MountKind::MaybeCard => {
                        if !pending.lock().unwrap().insert(bsd.clone()) {
                            continue;
                        }
                        let cmd = cmd.clone();
                        let pending = pending.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(SETTLE);
                            pending.lock().unwrap().remove(&bsd);
                            if !is_card(&path) {
                                return;
                            }
                            let card_id = card_identity(uuid.as_deref(), &path, &name, size);
                            let name = display_name(&name, &path);
                            let _ = cmd.send(Command::Enqueue(Job { card_id, bsd, mount: path, name, capacity: size }));
                        });
                    }
                }
            }
            VolumeEvent::Unmounted { bsd, .. } => {
                let _ = cmd.send(Command::CardRemoved { bsd });
            }
        }
    }
}

fn display_name(name: &str, path: &Path) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "未命名卡".into())
}

/// CLI 与测试用：手动把一个已挂载的卡路径变成 Job
pub fn job_for_path(path: PathBuf) -> Option<Job> {
    if !is_card(&path) {
        return None;
    }
    let info = crate::platform::volume_info(&path);
    let uuid = info.as_ref().and_then(|i| i.uuid.clone());
    let size = info.as_ref().map(|i| i.total_bytes).unwrap_or(0);
    let bsd = info.as_ref().and_then(|i| i.bsd.clone()).unwrap_or_default();
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Some(Job { card_id: card_identity(uuid.as_deref(), &path, &name, size), bsd, mount: path, name, capacity: size })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rules() {
        let cfg = Config {
            target_path: Some("/Volumes/Photos SSD/Cards".into()),
            target_volume_uuid: Some("T".into()),
            ..Default::default()
        };
        assert_eq!(classify(Some("X"), Path::new("/"), &cfg), MountKind::Ignore);
        assert_eq!(classify(Some("T"), Path::new("/Volumes/Photos SSD 1"), &cfg), MountKind::Target);
        assert_eq!(classify(None, Path::new("/Volumes/Photos SSD"), &cfg), MountKind::Target);
        assert_eq!(classify(Some("C"), Path::new("/Volumes/Untitled 1"), &cfg), MountKind::MaybeCard);
    }

    #[test]
    fn fallback_identity_is_stable() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("DCIM/100CANON")).unwrap();
        let a = card_identity(None, d.path(), "EOS_DIGITAL", 64);
        let b = card_identity(Some(""), d.path(), "EOS_DIGITAL", 64);
        assert_eq!(a, b);
        assert!(a.starts_with("fallback-"));
        assert_ne!(a, card_identity(None, d.path(), "EOS_DIGITAL", 128));
        assert_eq!(card_identity(Some("U-1"), d.path(), "x", 1), "U-1");
    }
}
